//! Local content-addressed models, native services, and supervised tasks.
use crate::{core::*, inventory::*, system::Operations};
use reqwest::{redirect::Policy, Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn hash_file(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink(),
        "model file is missing or a symlink",
    )?;
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn download_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .redirect(Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() >= 10
                || url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
            {
                attempt.error("model redirects must remain HTTPS without embedded credentials")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| Error::runtime("could not initialize HTTPS transport"))
}
/// Enforce one deadline around headers and every body chunk. A per-read timeout
/// alone would permit a trickling server to keep a transfer alive forever.
pub fn download_http(
    url: Url,
    output: &mut dyn Write,
    size: u64,
    timeout: Duration,
) -> Result<(u64, String)> {
    let client = download_client()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let transfer = async {
            let mut response = client.get(url).send().await.and_then(|r|r.error_for_status())
                .map_err(|_|Error::runtime("model transfer failed; existing content retained"))?;
            let mut hash = Sha256::new(); let mut total = 0u64;
            while let Some(chunk) = response.chunk().await.map_err(|_|Error::runtime("model body transfer failed"))? {
                total = total.checked_add(chunk.len() as u64).ok_or_else(||Error::runtime("download size overflow"))?;
                require(total <= size,"download exceeded declared size")?;
                output.write_all(&chunk)?;hash.update(&chunk);
            }
            Ok((total,format!("{:x}",hash.finalize())))
        };
        tokio::select! {
            result = transfer => result,
            _ = tokio::time::sleep(timeout) => Err(Error::runtime("model transfer deadline exceeded")),
            _ = async {loop {tokio::time::sleep(Duration::from_millis(50)).await;if interrupted(){break;}}} => Err(Error::runtime("model transfer interrupted")),
        }
    })
}
pub struct Resources<'a, 'b> {
    pub ops: &'a Operations<'b>,
}
impl<'a, 'b> Resources<'a, 'b> {
    pub fn new(ops: &'a Operations<'b>) -> Self {
        Self { ops }
    }
    pub fn model_path(&self, model: &Model) -> PathBuf {
        self.ops.state.root.join("models").join(&model.sha256)
    }
    fn model<'h>(&self, name: &str, host: &'h Host, resource: &str) -> Result<&'h Model> {
        self.ops.require_local(name, host)?;
        host.models
            .get(resource)
            .ok_or_else(|| Error::refusal(format!("unknown model: {resource}")))
    }
    pub fn models(&self, name: &str, host: &Host) -> Result<Value> {
        self.ops.require_local(name, host)?;
        Ok(json!(host.models.iter().map(|(name,m)| json!({"name": name, "sha256": m.sha256, "size": m.size,
            "path": self.model_path(m), "state": if self.model_path(m).is_file() { "present-unverified" } else { "missing" }})).collect::<Vec<_>>()))
    }
    pub fn verify(&self, name: &str, host: &Host, resource: &str) -> Result<Value> {
        let model = self.model(name, host, resource)?;
        let path = self.model_path(model);
        require(
            path.is_file(),
            format!("model {resource} is missing; run model fetch explicitly"),
        )?;
        require(
            hash_file(&path)? == model.sha256,
            format!("model {resource} failed SHA-256 verification"),
        )?;
        if let Some(size) = model.size {
            require(
                fs::metadata(&path)?.len() == size,
                "model has the wrong size",
            )?;
        }
        Ok(
            json!({"host": name, "model": resource, "verified": true, "path": path, "sha256": model.sha256}),
        )
    }
    pub fn fetch(&self, name: &str, host: &Host, resource: &str) -> Result<Value> {
        let model = self.model(name, host, resource)?;
        let _lock = self.ops.state.lock(&format!("model-{}", model.sha256))?;
        if let Ok(mut result) = self.verify(name, host, resource) {
            result["reused"] = json!(true);
            return Ok(result);
        }
        let raw = model.url.as_ref().ok_or_else(|| {
            Error::refusal("model has no retrieval source; declare a pinned HTTPS or file URL")
        })?;
        let url = Url::parse(raw).map_err(|_| Error::refusal("invalid model URL"))?;
        require(
            matches!(url.scheme(), "https" | "file")
                && url.username().is_empty()
                && url.password().is_none(),
            "models require HTTPS or a local file URL without embedded credentials",
        )?;
        let size = model
            .size
            .ok_or_else(|| Error::refusal("declare the model's byte size before downloading"))?;
        let needed = size
            .checked_add(1024 * 1024)
            .ok_or_else(|| Error::refusal("model size is too large"))?;
        let directory = self.ops.state.dir("models")?;
        require(
            fs2::available_space(&directory)? >= needed,
            "insufficient disk space for download staging",
        )?;
        progress(format!("{name}: fetching {resource}, {size} bytes"));
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        let (total, digest) = if url.scheme() == "file" {
            require(
                url.host_str().is_none_or(|h| h == "localhost"),
                "file source must be local",
            )?;
            let path = url
                .to_file_path()
                .map_err(|_| Error::refusal("file source must be an absolute path"))?;
            require(path.is_file(), "file source must be a regular file")?;
            let mut input = File::open(path)?;
            let mut hash = Sha256::new();
            let mut total = 0u64;
            let mut buffer = [0; 65536];
            loop {
                require(!interrupted(), "model transfer interrupted")?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                total = total
                    .checked_add(count as u64)
                    .ok_or_else(|| Error::runtime("download size overflow"))?;
                require(total <= size, "download exceeded declared size")?;
                staged.write_all(&buffer[..count])?;
                hash.update(&buffer[..count]);
            }
            (total, format!("{:x}", hash.finalize()))
        } else {
            download_http(url, &mut staged, size, Duration::from_secs(24 * 3600))?
        };
        require(total == size, "download did not match declared size")?;
        require(
            digest == model.sha256,
            "download failed SHA-256 verification",
        )?;
        staged.as_file().sync_all()?;
        staged
            .persist(self.model_path(model))
            .map_err(|e| Error::runtime(e.to_string()))?;
        Ok(
            json!({"host": name, "model": resource, "verified": true, "reused": false, "path": self.model_path(model), "sha256": model.sha256}),
        )
    }
    fn service_spec<'h>(&self, name: &str, host: &'h Host, resource: &str) -> Result<&'h Service> {
        self.ops.require_local(name, host)?;
        let service = host
            .services
            .get(resource)
            .ok_or_else(|| Error::refusal(format!("unknown service: {resource}")))?;
        require(
            (host.platform == Platform::Darwin) == (service.manager == Manager::Launchd),
            "service manager does not match host platform",
        )?;
        Ok(service)
    }
    fn domain(service: &Service) -> String {
        format!(
            "{}/{}",
            if service.scope == Scope::System {
                "system".into()
            } else {
                format!("gui/{}", uid())
            },
            service.unit
        )
    }
    pub fn service_status(
        &self,
        name: &str,
        host: &Host,
        resource: &str,
        probe: bool,
    ) -> Result<Value> {
        let service = self.service_spec(name, host, resource)?;
        let (loaded, running, state) = if service.manager == Manager::Systemd {
            let mut args = vec!["systemctl".to_string()];
            if service.scope == Scope::User {
                args.push("--user".into());
            }
            args.extend(
                [
                    "show",
                    "--property=LoadState,ActiveState,SubState,UnitFileState",
                    "--",
                    &service.unit,
                ]
                .map(String::from),
            );
            let output = self.ops.runner.run(Invocation::new(args).timeout(5))?;
            let fields: std::collections::BTreeMap<_, _> = output
                .stdout
                .lines()
                .filter_map(|l| l.split_once('='))
                .collect();
            require(
                fields.contains_key("LoadState") && fields.contains_key("ActiveState"),
                "systemctl returned no service state",
            )?;
            (
                fields["LoadState"] == "loaded",
                fields["ActiveState"] == "active",
                fields["ActiveState"].to_string(),
            )
        } else {
            let output = self.ops.runner.run(
                Invocation::new(["launchctl", "print", &Self::domain(service)])
                    .timeout(5)
                    .unchecked(),
            )?;
            let loaded = output.code == 0;
            let running = loaded && output.stdout.lines().any(|l| l.trim() == "state = running");
            (
                loaded,
                running,
                if running {
                    "running"
                } else if loaded {
                    "loaded"
                } else {
                    "unavailable"
                }
                .into(),
            )
        };
        let mut ready = None;
        if probe && running {
            if let Some(args) = &service.ready {
                ready = Some(
                    match self
                        .ops
                        .runner
                        .run(Invocation::new(args.clone()).timeout(5).unchecked())
                    {
                        Ok(output) => output.code == 0,
                        Err(e) if e.code == 1 => false,
                        Err(e) => return Err(e),
                    },
                );
            }
        }
        Ok(
            json!({"host": name, "service": resource, "loaded": loaded, "running": running, "state": state, "ready": ready, "bootPolicy": service.boot}),
        )
    }
    pub fn service(&self, name: &str, host: &Host, resource: &str, action: &str) -> Result<Value> {
        let service = self.service_spec(name, host, resource)?;
        require(
            matches!(action, "start" | "stop" | "restart"),
            "invalid service operation",
        )?;
        if action != "stop" {
            for model in &service.models {
                self.verify(name, host, model)?;
            }
        }
        let current = self.service_status(name, host, resource, false)?;
        require(
            current["loaded"] == true,
            "service is not loaded; install its declaration first",
        )?;
        let mut args: Vec<String> = if service.scope == Scope::System {
            vec!["sudo".into(), "--".into()]
        } else {
            vec![]
        };
        if service.manager == Manager::Systemd {
            args.push("systemctl".into());
            if service.scope == Scope::User {
                args.push("--user".into());
            }
            args.extend([action, "--", &service.unit].map(String::from));
        } else if action == "stop" {
            require(
                service.restart_on_exit == Some(false),
                "launchd stop requires explicit restartOnExit = false",
            )?;
            args.extend([
                "launchctl".into(),
                "kill".into(),
                "SIGTERM".into(),
                Self::domain(service),
            ]);
        } else {
            args.extend(["launchctl".into(), "kickstart".into()]);
            if action == "restart" {
                args.push("-k".into());
            }
            args.push(Self::domain(service));
        }
        self.ops
            .runner
            .run(Invocation::new(args).stream(Stream::Progress))?;
        let until = Instant::now() + Duration::from_secs_f64(service.timeout);
        loop {
            let mut status = self.service_status(name, host, resource, action != "stop")?;
            if (action == "stop" && status["running"] == false)
                || (action != "stop" && status["running"] == true && status["ready"] != false)
            {
                status["note"] =
                    json!("runtime operation only; boot and schedule policy unchanged");
                return Ok(status);
            }
            if interrupted() || Instant::now() >= until {
                return Err(Error::runtime("service did not reach the requested state"));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub fn service_logs(
        &self,
        name: &str,
        host: &Host,
        resource: &str,
        follow: bool,
    ) -> Result<()> {
        let service = self.service_spec(name, host, resource)?;
        require(
            service.manager == Manager::Systemd,
            "launchd log streaming is not implemented; use native log tools",
        )?;
        let mut args = vec!["journalctl".into()];
        if service.scope == Scope::User {
            args.push("--user".into());
        }
        args.extend(["--unit", &service.unit, "--no-pager", "--lines", "100"].map(String::from));
        if follow {
            args.push("--follow".into());
        }
        self.ops
            .runner
            .run(Invocation::new(args).stream(Stream::Stdout).unlimited())?;
        Ok(())
    }
    fn task<'h>(&self, name: &str, host: &'h Host, resource: &str) -> Result<&'h Task> {
        self.ops.require_local(name, host)?;
        host.tasks
            .get(resource)
            .ok_or_else(|| Error::refusal(format!("unknown task: {resource}")))
    }
    pub fn task_list(&self, name: &str, host: &Host) -> Result<Value> {
        self.ops.require_local(name, host)?;
        Ok(json!(host.tasks.iter().map(|(n,t)| json!({"name": n, "models": t.models, "services": t.services, "timeout": t.timeout, "schedule": "not managed by Rig"})).collect::<Vec<_>>()))
    }
    pub fn task_runs(&self, name: &str, host: &Host, resource: &str) -> Result<Vec<Value>> {
        self.task(name, host, resource)?;
        let directory = self.ops.state.root.join("runs");
        if !directory.exists() {
            return Ok(vec![]);
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let run = entry.file_name().to_string_lossy().into_owned();
            if !run.starts_with("run-") {
                continue;
            }
            if let Some(mut record) = self.ops.state.read(&format!("runs/{run}/record.json"))? {
                if record["host"] != name || record["task"] != resource {
                    continue;
                }
                if matches!(record["state"].as_str(), Some("starting" | "running")) {
                    if let Ok(_lock) = self.ops.state.lock(&format!("task-{name}-{resource}")) {
                        record["recordedState"] = record["state"].clone();
                        record["state"] = json!("unknown");
                        record["note"] = json!("CLI supervisor is absent; inspect before retrying; PID may have been reused");
                    }
                }
                records.push(record);
            }
        }
        records.sort_by(|a, b| b["startedAt"].as_str().cmp(&a["startedAt"].as_str()));
        Ok(records)
    }
    pub fn task_run(&self, name: &str, host: &Host, resource: &str) -> Result<Value> {
        let task = self.task(name, host, resource)?;
        if let Some(user) = &task.user {
            require(&user_name()? == user, "tasks run as the invoking user only")?;
        }
        require(task.cwd.is_dir(), "task working directory is missing")?;
        let _lock = self.ops.state.lock(&format!("task-{name}-{resource}"))?;
        for prior in self.task_runs(name, host, resource)? {
            require(
                !matches!(
                    prior["state"].as_str(),
                    Some("starting" | "running" | "unknown")
                ),
                format!(
                    "unfinished run {} needs manual inspection before retrying",
                    prior["id"]
                ),
            )?;
        }
        for model in &task.models {
            self.verify(name, host, model)?;
        }
        for service in &task.services {
            let status = self.service_status(name, host, service, true)?;
            require(
                status["running"] == true && status["ready"] != false,
                format!("required service is not ready: {service}"),
            )?;
        }
        for (secret, path) in &task.secrets {
            require(
                path.is_file() && File::open(path).is_ok(),
                format!("required secret unavailable: {secret}"),
            )?;
        }
        let run = id("run");
        let directory = self.ops.state.dir(format!("runs/{run}"))?;
        let mut record = json!({"id": run, "host": name, "task": resource, "state": "starting", "startedAt": now(), "log": directory.join("output.log"), "artifacts": directory});
        let record_path = format!("runs/{run}/record.json");
        self.ops.state.write(&record_path, &record)?;
        progress(format!(
            "{name}: {resource}, run {run}; output in {}",
            directory.join("output.log").display()
        ));
        let run_result = (|| -> Result<()> {
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join("output.log"))?;
            let child = Command::new(&task.argv[0])
                .args(&task.argv[1..])
                .current_dir(&task.cwd)
                .env("RIG_RUN_ID", &run)
                .env("RIG_OUTPUT_DIR", &directory)
                .stdin(Stdio::null())
                .stderr(Stdio::from(output.try_clone()?))
                .stdout(Stdio::from(output))
                .process_group(0)
                .spawn()?;
            let mut child = ChildGuard::new(child, true);
            record["state"] = json!("running");
            record["pid"] = json!(child.id());
            if let Err(error) = self.ops.state.write(&record_path, &record) {
                let _ = stop_group(&mut child, Duration::from_secs(2));
                return Err(error);
            }
            let until = Instant::now() + Duration::from_secs_f64(task.timeout);
            loop {
                if let Some(status) = child.try_wait()? {
                    stop_group(&mut child, Duration::from_millis(300))?;
                    child.armed = false;
                    record["state"] = json!(if status.success() {
                        "succeeded"
                    } else {
                        "failed"
                    });
                    record["exitCode"] = json!(status.code());
                    break;
                }
                if interrupted() || Instant::now() >= until {
                    stop_group(&mut child, Duration::from_millis(300))?;
                    child.armed = false;
                    record["state"] = json!(if interrupted() {
                        "cancelled"
                    } else {
                        "timed-out"
                    });
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })();
        if let Err(error) = run_result {
            record["state"] = json!("failed");
            record["error"] = json!(error.message);
        }
        record["finishedAt"] = json!(now());
        self.ops.state.write(&record_path, &record)?;
        if record["state"] != "succeeded" {
            return Err(
                Error::runtime(format!("task {resource} {}", record["state"])).with_details(record),
            );
        }
        Ok(record)
    }
    pub fn task_logs(&self, name: &str, host: &Host, run: &str, follow: bool) -> Result<()> {
        self.ops.require_local(name, host)?;
        require(
            run.len() == 20
                && run.starts_with("run-")
                && run[4..].bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid task run ID",
        )?;
        let record = self
            .ops
            .state
            .read(&format!("runs/{run}/record.json"))?
            .ok_or_else(|| Error::refusal("run not found"))?;
        require(record["host"] == name, "run was not found on this host")?;
        let mut args = vec!["tail".into(), "-n".into(), "100".into()];
        if follow {
            args.push("-f".into());
        }
        args.extend([
            "--".into(),
            self.ops
                .state
                .root
                .join(format!("runs/{run}/output.log"))
                .to_string_lossy()
                .into_owned(),
        ]);
        self.ops
            .runner
            .run(Invocation::new(args).stream(Stream::Stdout).unlimited())?;
        Ok(())
    }
}
fn user_name() -> Result<String> {
    let mut buffer = vec![0u8; 65536];
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid(),
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    require(
        code == 0 && !result.is_null(),
        "cannot resolve invoking user",
    )?;
    Ok(unsafe { std::ffi::CStr::from_ptr((*result).pw_name) }
        .to_string_lossy()
        .into_owned())
}
