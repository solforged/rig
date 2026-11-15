//! Process boundaries and private state. Native stderr is never copied into errors.
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    os::{
        fd::AsFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Error {
    pub message: String,
    pub code: i32,
    pub details: Value,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(message: impl Into<String>, code: i32) -> Self {
        Self {
            message: message.into(),
            code,
            details: json!({}),
        }
    }
    pub fn refusal(message: impl Into<String>) -> Self {
        Self::new(message, 2)
    }
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(message, 1)
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::runtime(e.to_string())
    }
}
pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::refusal(message))
    }
}
/// Diagnostics only. A vanished terminal (hangup) must not abort cleanup.
pub fn progress(message: impl fmt::Display) {
    let _ = writeln!(io::stderr(), "rig: {message}");
}
pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("valid timestamp")
}
pub fn id(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    )
}
pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::refusal("HOME is not set"))
}
pub fn safe_name(name: &str) -> Result<()> {
    require(
        !name.is_empty()
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
        format!("invalid resource name: {name:?}"),
    )
}
pub fn store_path(path: &str) -> Result<&str> {
    let tail = path.strip_prefix("/nix/store/").unwrap_or("");
    require(
        tail.len() > 33
            && tail.as_bytes().get(32) == Some(&b'-')
            && tail.as_bytes()[..32.min(tail.len())]
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && tail.as_bytes()[33..]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"+-._?=".contains(c)),
        format!("backend returned an invalid store path: {path:?}"),
    )?;
    Ok(path)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Capture,
    Progress,
    Stdout,
}
/// How a native command reacts once the operator has cancelled with Ctrl-C,
/// SIGTERM, or terminal hangup. Cancellation is sticky for the life of the
/// process and is never cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cancellation {
    /// Ordinary work: not started after cancellation, stopped when cancelled mid-run.
    Honor,
    /// Cleanup of Rig's own state after cancellation. It still starts and runs,
    /// but the bound applies even when the invocation asks to be unlimited.
    Cleanup(Duration),
}
#[derive(Clone, Debug)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub stream: Stream,
    pub check: bool,
    pub env: BTreeMap<String, String>,
    pub cancellation: Cancellation,
}
impl Invocation {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            argv: args.into_iter().map(Into::into).collect(),
            cwd: None,
            timeout: Some(Duration::from_secs(60)),
            stream: Stream::Capture,
            check: true,
            env: BTreeMap::new(),
            cancellation: Cancellation::Honor,
        }
    }
    pub fn cwd(mut self, path: impl AsRef<Path>) -> Self {
        self.cwd = Some(path.as_ref().into());
        self
    }
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(Duration::from_secs(seconds));
        self
    }
    /// Sub-second precision for declared deadlines.
    pub fn time_limit(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }
    pub fn unlimited(mut self) -> Self {
        self.timeout = None;
        self
    }
    /// Run despite an earlier cancellation, for at most `bound`.
    pub fn cleanup(mut self, bound: Duration) -> Self {
        self.cancellation = Cancellation::Cleanup(bound);
        self
    }
    /// The longest the command may run. Cleanup is never unbounded.
    pub fn limit(&self) -> Option<Duration> {
        match self.cancellation {
            Cancellation::Honor => self.timeout,
            Cancellation::Cleanup(bound) => Some(self.timeout.map_or(bound, |t| t.min(bound))),
        }
    }
    pub fn stream(mut self, stream: Stream) -> Self {
        self.stream = stream;
        self
    }
    pub fn unchecked(mut self) -> Self {
        self.check = false;
        self
    }
}
#[derive(Debug, Clone)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
pub trait Runner: Send + Sync {
    fn run(&self, command: Invocation) -> Result<Output>;
    fn active_system(&self, path: &Path) -> Result<Option<String>> {
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(path.canonicalize()?.to_string_lossy().into_owned()))
    }
}
pub fn run_json(runner: &dyn Runner, command: Invocation) -> Result<Value> {
    let result = runner.run(command)?;
    serde_json::from_str(&result.stdout)
        .map_err(|_| Error::runtime("backend returned invalid JSON"))
}

static INTERRUPTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
/// Ctrl-C, termination, and terminal hangup request the same sticky
/// cancellation, so each supervisor takes its normal stop-and-record path.
pub fn install_signals() -> Result<()> {
    let flag = INTERRUPTED.get_or_init(|| Arc::new(AtomicBool::new(false)));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, flag.clone())?;
    }
    Ok(())
}
pub fn interrupted() -> bool {
    INTERRUPTED.get().is_some_and(|f| f.load(Ordering::Relaxed))
}
/// Call before starting work and between phases; `message` names what was prevented.
pub fn ensure_uninterrupted(message: &str) -> Result<()> {
    if interrupted() {
        Err(Error::runtime(message))
    } else {
        Ok(())
    }
}
pub struct NativeRunner;
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}
fn read_pipe(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    rx
}
fn signal_group(pid: u32, signal: i32) -> io::Result<bool> {
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}
/// A group can outlive its leader. Check the group, not just Child::wait().
pub fn stop_group(child: &mut Child, grace: Duration) -> Result<()> {
    let pid = child.id();
    if !signal_group(pid, libc::SIGTERM)? {
        let _ = child.wait();
        return Ok(());
    }
    let until = Instant::now() + grace;
    while Instant::now() < until {
        let _ = child.try_wait();
        if !signal_group(pid, 0)? {
            let _ = child.wait();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    signal_group(pid, libc::SIGKILL)?;
    let _ = child.wait();
    Ok(())
}
/// Cleans up an interrupted supervisor even when a filesystem or pipe operation fails.
pub struct ChildGuard {
    pub child: Child,
    pub armed: bool,
    pub grouped: bool,
}
impl ChildGuard {
    pub fn new(child: Child, grouped: bool) -> Self {
        Self {
            child,
            armed: true,
            grouped,
        }
    }
}
impl std::ops::Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.grouped {
            let _ = stop_group(&mut self.child, Duration::from_millis(300));
        } else {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Runner for NativeRunner {
    fn run(&self, spec: Invocation) -> Result<Output> {
        require(!spec.argv.is_empty(), "empty native command")?;
        let program = &spec.argv[0];
        // Cancellation is sticky. Ordinary commands never start after it;
        // bounded cleanup is the only exception.
        let cancellable = spec.cancellation == Cancellation::Honor;
        if cancellable && interrupted() {
            return Err(Error::runtime(format!(
                "{program} was not started: operation interrupted"
            )));
        }
        let limit = spec.limit();
        let mut command = Command::new(program);
        command.args(&spec.argv[1..]).envs(&spec.env);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        let captured = spec.stream == Stream::Capture;
        // Interactive elevation must stay in the foreground terminal group.
        // Headless streamed commands can be supervised just like captured ones.
        let grouped = captured || !io::stdin().is_terminal();
        if grouped {
            command.process_group(0);
        }
        if captured {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        } else {
            command.stdin(Stdio::inherit()).stderr(Stdio::inherit());
            command.stdout(if spec.stream == Stream::Stdout {
                Stdio::inherit()
            } else {
                Stdio::from(io::stderr().as_fd().try_clone_to_owned()?)
            });
        }
        let child = command.spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                Error::refusal(format!("required executable is missing: {program}"))
            } else {
                Error::runtime(format!("could not run {program}: {e}"))
            }
        })?;
        let mut child = ChildGuard::new(child, grouped);
        let stdout = child.stdout.take().map(read_pipe);
        let stderr = child.stderr.take().map(read_pipe);
        let mut out = if captured { None } else { Some(Vec::new()) };
        let mut err = if captured { None } else { Some(Vec::new()) };
        let started = Instant::now();
        loop {
            if let Some(rx) = &stdout {
                if out.is_none() {
                    if let Ok(bytes) = rx.try_recv() {
                        out = Some(bytes?);
                    }
                }
            }
            if let Some(rx) = &stderr {
                if err.is_none() {
                    if let Ok(bytes) = rx.try_recv() {
                        err = Some(bytes?);
                    }
                }
            }
            if let Some(status) = child.try_wait()? {
                if let (Some(out), Some(err)) = (&out, &err) {
                    let code = exit_code(status);
                    child.armed = false;
                    if spec.check && code != 0 {
                        return Err(Error::runtime(format!(
                            "{program} exited with status {code}"
                        )));
                    }
                    return Ok(Output {
                        code,
                        stdout: String::from_utf8_lossy(out).into(),
                        stderr: String::from_utf8_lossy(err).into(),
                    });
                }
            }
            if (cancellable && interrupted())
                || limit.is_some_and(|limit| started.elapsed() >= limit)
            {
                if grouped {
                    stop_group(&mut child, Duration::from_millis(300))?;
                } else {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                child.armed = false;
                return Err(Error::runtime(format!(
                    "{program} interrupted or timed out; inspect state before retrying"
                )));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Clone)]
pub struct State {
    pub root: PathBuf,
}
pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
impl State {
    pub fn from_env() -> Result<Self> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/state"));
        Self::new(base.join("rig"))
    }
    pub fn new(root: PathBuf) -> Result<Self> {
        require(root.is_absolute(), "Rig state must have an absolute path")?;
        Ok(Self { root })
    }
    pub fn dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        require(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
            "invalid state directory",
        )?;
        if let Some(parent) = self.root.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut path = self.root.clone();
        for part in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(part) = part {
                path.push(part);
            }
            if let Ok(meta) = fs::symlink_metadata(&path) {
                require(
                    !meta.file_type().is_symlink(),
                    "state directories must not be symlinks",
                )?;
            }
            match fs::create_dir(&path) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
            require(
                fs::metadata(&path)?.uid() == uid(),
                "state directory belongs to a different user",
            )?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(path)
    }
    pub fn write(&self, relative: &str, value: &Value) -> Result<()> {
        let path = self.root.join(relative);
        self.dir(Path::new(relative).parent().unwrap_or(Path::new("")))?;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        serde_json::to_writer_pretty(&mut file, value)
            .map_err(|e| Error::runtime(e.to_string()))?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)
            .map_err(|e| Error::runtime(e.to_string()))?;
        Ok(())
    }
    pub fn read(&self, relative: &str) -> Result<Option<Value>> {
        let path = self.root.join(relative);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| Error::runtime(format!("invalid state file: {}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn lock(&self, name: &str) -> Result<Lock> {
        safe_name(name)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.dir("locks")?.join(name))?;
        file.try_lock_exclusive()
            .map_err(|_| Error::refusal(format!("another Rig operation holds the {name} lock")))?;
        Ok(Lock(file))
    }
}
pub fn confirm(message: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    require(
        io::stdin().is_terminal(),
        format!("{message}; confirmation needs a terminal or --yes"),
    )?;
    eprint!("{message} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    require(
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        "operation cancelled",
    )
}
