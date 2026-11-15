#![allow(dead_code)]
use rig::{core::*, inventory::*, system::Operations};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

pub const SOURCE: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source";
pub const CLOSURE: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-system";
pub const OLD: &str = "/nix/store/cccccccccccccccccccccccccccccccc-system";
pub fn inventory() -> Value {
    json!({"version":1,"hosts":{
    "local":{"platform":local_platform(),"managed":true},
    "remote":{"platform":"nixos","managed":true,"ssh":{"host":"remote.example","user":"operator"}},
    "work":{"platform":"darwin","managed":false,"owner":"private work flake"}},
    "secrets":{"mail":{"recipients":["age1public"]}}})
}
pub struct FakeRunner {
    pub data: Mutex<Value>,
    pub calls: Mutex<Vec<Invocation>>,
    pub active: Mutex<Option<String>>,
    pub fail: Mutex<Option<String>>,
    pub after_switch: Mutex<String>,
    pub switch_fails: Mutex<bool>,
    pub service_running: Mutex<bool>,
    pub probe_fails: Mutex<bool>,
    pub jj_config: Option<PathBuf>,
    pub noop: Mutex<bool>,
    pub concurrent_edit: Mutex<Option<(PathBuf, String)>>,
}
impl FakeRunner {
    pub fn new(data: Value) -> Self {
        Self {
            data: Mutex::new(data),
            calls: Mutex::new(vec![]),
            active: Mutex::new(Some(OLD.into())),
            fail: Mutex::new(None),
            after_switch: Mutex::new(CLOSURE.into()),
            switch_fails: Mutex::new(false),
            service_running: Mutex::new(true),
            probe_fails: Mutex::new(false),
            jj_config: None,
            noop: Mutex::new(false),
            concurrent_edit: Mutex::new(None),
        }
    }
    pub fn commands(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.argv.clone())
            .collect()
    }
    pub fn did(&self, text: &str) -> bool {
        self.commands().iter().any(|a| a.join(" ").contains(text))
    }
}
impl Runner for FakeRunner {
    fn active_system(&self, _: &Path) -> Result<Option<String>> {
        Ok(self.active.lock().unwrap().clone())
    }
    fn run(&self, mut command: Invocation) -> Result<Output> {
        self.calls.lock().unwrap().push(command.clone());
        if command.argv[0] == "jj" {
            if let Some(config) = &self.jj_config {
                command
                    .env
                    .insert("JJ_CONFIG".into(), config.to_string_lossy().into());
            }
            let check = command.check;
            command.check = false;
            let result = NativeRunner.run(command)?;
            if check && result.code != 0 {
                return Err(Error::runtime(format!(
                    "fixture jj failed: {}",
                    result.stderr
                )));
            }
            return Ok(result);
        }
        let text = command.argv.join(" ");
        if self
            .fail
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| text.contains(s))
        {
            return Err(Error::runtime("fake backend failed"));
        }
        let a = &command.argv;
        let mut stdout = String::new();
        let mut code = 0;
        if a[0] == "nh" {
            if a.contains(&"--help".into()) {
                stdout = "Path or symlink to a /nix/store path --diff".into();
            } else {
                *self.active.lock().unwrap() = Some(self.after_switch.lock().unwrap().clone());
                if *self.switch_fails.lock().unwrap() {
                    return Err(Error::runtime("activation transport failed"));
                }
            }
        } else if a[0] == "ssh" {
            stdout = self.active.lock().unwrap().clone().unwrap_or_default();
        } else if a.starts_with(&["nix".into(), "flake".into(), "archive".into()]) {
            stdout = json!({"path":SOURCE,"inputs":{}}).to_string();
        } else if a.starts_with(&["nix".into(), "hash".into(), "path".into()]) {
            stdout = "sha256-test\n".into();
        } else if a.starts_with(&["nix".into(), "flake".into(), "update".into()]) {
            if !*self.noop.lock().unwrap() {
                let p = command.cwd.as_ref().unwrap().join("flake.lock");
                let mut lock: Value = serde_json::from_slice(&fs::read(&p)?).unwrap();
                lock["nodes"]["nixpkgs"]["locked"] = json!({"rev":"new","narHash":"sha256-new"});
                fs::write(p, lock.to_string())?;
            }
        } else if a.starts_with(&["nix".into(), "eval".into()]) {
            if a.contains(&"--raw".into()) {
                if let Some((path, text)) = self.concurrent_edit.lock().unwrap().take() {
                    fs::write(path, text)?;
                }
                stdout = "/nix/store/fake.drv".into();
            } else if text.contains("checks = f.checks") {
                stdout = "{}".into();
            } else {
                stdout = self.data.lock().unwrap().to_string();
            }
        } else if a.starts_with(&["nix".into(), "build".into()]) {
            stdout = json!([{"outputs":{"out":CLOSURE}}]).to_string();
        } else if a.starts_with(&["nix".into(), "store".into(), "diff-closures".into()]) {
            stdout = "package 1 -> 2\n".into();
        } else if a.contains(&"--print-dead".into()) {
            stdout = format!("{CLOSURE}\n");
        } else if a.contains(&"systemctl".into()) {
            if a.contains(&"show".into()) {
                stdout = format!(
                    "LoadState=loaded\nActiveState={}\nSubState=running\n",
                    if *self.service_running.lock().unwrap() {
                        "active"
                    } else {
                        "inactive"
                    }
                );
            } else if a.contains(&"stop".into()) {
                *self.service_running.lock().unwrap() = false;
            }
        } else if a.contains(&"launchctl".into()) {
            if a.contains(&"print".into()) {
                stdout = if *self.service_running.lock().unwrap() {
                    "state = running\n"
                } else {
                    "state = waiting\n"
                }
                .into();
            }
            if a.contains(&"kill".into()) {
                *self.service_running.lock().unwrap() = false;
            }
        } else if a[0].contains("probe") && *self.probe_fails.lock().unwrap() {
            return Err(Error::runtime("probe timed out"));
        } else if a[0].contains("false") {
            code = 1;
        }
        Ok(Output {
            code,
            stdout,
            stderr: String::new(),
        })
    }
}
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub flake: PathBuf,
    pub runner: FakeRunner,
    pub inventory: Inventory,
    pub state: State,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_data(inventory())
    }
    pub fn with_data(data: Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let flake = temp.path().join("flake");
        fs::create_dir(&flake).unwrap();
        fs::write(flake.join("flake.nix"), "{}\n").unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let inv = Inventory::parse(data.clone()).unwrap();
        Self {
            temp,
            flake,
            runner: FakeRunner::new(data),
            inventory: inv,
            state,
        }
    }
    pub fn ops(&self) -> Operations<'_> {
        Operations {
            flake: self.flake.clone(),
            inventory: self.inventory.clone(),
            runner: &self.runner,
            state: self.state.clone(),
            names: BTreeSet::from(["local".into()]),
            current_system: "/unused-test-system".into(),
        }
    }
    pub fn host(&self) -> &Host {
        &self.inventory.hosts["local"]
    }
}
pub fn shell() -> PathBuf {
    rig::system::find_executable("sh").expect("sh is required for process tests")
}
pub fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!{}\n{body}\n", shell().display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
/// Polls an observable condition; never a fixed sleep standing in for one.
pub fn wait_until(what: &str, limit: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}
/// Cancellation is sticky for a process, so tests that trip it re-run
/// themselves alone in a child copy of this test binary. Returns true only
/// inside that child, where the test body should proceed.
pub fn isolated(test: &str) -> bool {
    if std::env::var("RIG_ISOLATED_TEST").is_ok_and(|v| v == test) {
        return true;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env("RIG_ISOLATED_TEST", test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated test {test} failed ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
/// Delivers a real signal to this process and waits until Rig's handlers have
/// recorded the cancellation.
pub fn cancel_with(signal: i32) {
    install_signals().unwrap();
    assert_eq!(unsafe { libc::kill(libc::getpid(), signal) }, 0);
    wait_until(
        "the signal to be recorded",
        Duration::from_secs(5),
        interrupted,
    );
}
/// A name this machine answers to, exactly as the CLI selects local hosts.
pub fn local_name() -> String {
    local_names()
        .unwrap()
        .into_iter()
        .filter(|name| safe_name(name).is_ok())
        .min_by_key(String::len)
        .expect("this machine needs a host name Rig accepts")
}
/// A `rig` command wired to a fake `nix` that serves `inventory`, with HOME,
/// state and PATH confined to the fixture. `nix flake update` execs the script
/// named by `RIG_FAKE_UPDATE`.
pub fn rig_command(f: &Fixture, inventory: Value) -> Command {
    let bin = f.temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let inventory_file = f.temp.path().join("inventory.json");
    fs::write(&inventory_file, inventory.to_string()).unwrap();
    write_script(
        &bin.join("nix"),
        r#"printf 'CALL %s\n' "$*" >> "$RIG_FAKE_LOG"
case "$1:$2" in
  eval:*) cat "$RIG_FAKE_INVENTORY" ;;
  flake:archive) printf '%s\n' "$RIG_FAKE_SOURCE" ;;
  flake:update) exec "$RIG_FAKE_UPDATE" ;;
  hash:path) printf 'sha256-test\n' ;;
  *) exit 99 ;;
esac"#,
    );
    let home = f.temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rig"));
    command
        .arg("--flake")
        .arg(&f.flake)
        .env("PATH", path)
        .env("HOME", home)
        .env("RIG_FAKE_INVENTORY", inventory_file)
        .env("RIG_FAKE_SOURCE", json!({"path": SOURCE}).to_string())
        .env("RIG_FAKE_LOG", f.temp.path().join("calls"))
        .env("XDG_STATE_HOME", f.temp.path().join("state-home"));
    command
}
/// Starts a long-lived `rig` child whose exit the test controls.
pub fn spawn(mut command: Command) -> Child {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
pub fn signal(child: &Child, signal: i32) {
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
}
/// Waits for the child to exit; a hang kills it and fails the test.
pub fn finish(mut child: Child, limit: Duration) -> std::process::Output {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not exit within {limit:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}
