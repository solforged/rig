mod common;
use common::*;
use rig::{
    core::{Invocation, NativeRunner, Output, Result, Runner, Stream},
    inventory::*,
    resources::*,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn model_fixture() -> Fixture {
    let mut f = Fixture::new();
    let blob = f.temp.path().join("weights");
    let content = b"small pinned model fixture";
    fs::write(&blob, content).unwrap();
    f.inventory.hosts.get_mut("local").unwrap().models.insert(
        "triage".into(),
        Model {
            sha256: format!("{:x}", Sha256::digest(content)),
            url: Some(reqwest::Url::from_file_path(blob).unwrap().to_string()),
            size: Some(content.len() as u64),
        },
    );
    f
}
#[test]
fn model_fetch_verifies_and_reuses() {
    let f = model_fixture();
    let ops = f.ops();
    let r = Resources::new(&ops);
    let first = r.fetch("local", f.host(), "triage").unwrap();
    let second = r.fetch("local", f.host(), "triage").unwrap();
    assert_eq!(first["reused"], false);
    assert_eq!(second["reused"], true);
    let path = std::path::Path::new(first["path"].as_str().unwrap());
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        r.verify("local", f.host(), "triage").unwrap()["verified"],
        true
    );
}
#[test]
fn bad_transfer_retains_existing_content() {
    let f = model_fixture();
    let ops = f.ops();
    let r = Resources::new(&ops);
    let first = r.fetch("local", f.host(), "triage").unwrap();
    let path = std::path::Path::new(first["path"].as_str().unwrap());
    fs::write(path, b"old corrupt copy").unwrap();
    fs::write(f.temp.path().join("weights"), b"xxxxxxxxxxxxxxxxxxxxxxxxxx").unwrap();
    assert!(r.fetch("local", f.host(), "triage").is_err());
    assert_eq!(fs::read(path).unwrap(), b"old corrupt copy");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}
#[test]
fn download_enforces_size() {
    let f = model_fixture();
    fs::write(f.temp.path().join("weights"), vec![0; 100]).unwrap();
    assert!(Resources::new(&f.ops())
        .fetch("local", f.host(), "triage")
        .is_err());
}
#[test]
fn verify_detects_tampering() {
    let f = model_fixture();
    let ops = f.ops();
    let r = Resources::new(&ops);
    let value = r.fetch("local", f.host(), "triage").unwrap();
    fs::write(value["path"].as_str().unwrap(), b"tampered").unwrap();
    assert!(r.verify("local", f.host(), "triage").is_err());
}
#[test]
fn model_symlink_is_replaced_not_followed() {
    let f = model_fixture();
    let ops = f.ops();
    let r = Resources::new(&ops);
    let victim = f.temp.path().join("victim");
    fs::write(&victim, b"do not overwrite").unwrap();
    let path = f
        .state
        .dir("models")
        .unwrap()
        .join(&f.host().models["triage"].sha256);
    symlink(&victim, &path).unwrap();
    assert!(r.verify("local", f.host(), "triage").is_err());
    r.fetch("local", f.host(), "triage").unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"do not overwrite");
    assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
}
#[test]
fn transfer_rejects_http_credentials_and_remote_file_hosts() {
    for url in [
        "http://example.org/model",
        "https://user:secret@example.org/model",
        "file://remote/path",
    ] {
        let mut f = model_fixture();
        f.inventory
            .hosts
            .get_mut("local")
            .unwrap()
            .models
            .get_mut("triage")
            .unwrap()
            .url = Some(url.into());
        assert!(Resources::new(&f.ops())
            .fetch("local", f.host(), "triage")
            .is_err());
    }
}
#[test]
fn fetching_requires_source_and_size() {
    let mut f = model_fixture();
    f.inventory
        .hosts
        .get_mut("local")
        .unwrap()
        .models
        .get_mut("triage")
        .unwrap()
        .size = None;
    assert!(Resources::new(&f.ops())
        .fetch("local", f.host(), "triage")
        .is_err());
}
#[test]
fn model_list_never_claims_content_verified() {
    let f = model_fixture();
    let ops = f.ops();
    let r = Resources::new(&ops);
    r.fetch("local", f.host(), "triage").unwrap();
    assert_eq!(
        r.models("local", f.host()).unwrap()[0]["state"],
        "present-unverified"
    );
}
#[test]
fn transfer_deadline_applies_to_trickling_body() {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e)
            if std::env::var_os("NIX_BUILD_TOP").is_some()
                && e.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            return
        }
        Err(e) => panic!("{e}"),
    };
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut input = [0; 4096];
        let _ = stream.read(&mut input);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n");
        for _ in 0..100 {
            if stream.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let start = Instant::now();
    let mut output = Vec::new();
    let result = download_http(
        reqwest::Url::parse(&format!("http://{address}")).unwrap(),
        &mut output,
        100,
        Duration::from_millis(100),
    );
    assert!(result.unwrap_err().message.contains("deadline"));
    assert!(start.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}
fn service_fixture() -> Fixture {
    let mut f = Fixture::new();
    let manager = if local_platform() == Platform::Darwin {
        Manager::Launchd
    } else {
        Manager::Systemd
    };
    f.inventory.hosts.get_mut("local").unwrap().services.insert(
        "mail".into(),
        Service {
            manager,
            scope: Scope::User,
            unit: "dev.mail".into(),
            ready: Some(vec!["/fake/probe".into()]),
            timeout: 0.05,
            models: vec![],
            boot: Some(false),
            restart_on_exit: Some(false),
        },
    );
    f
}
#[test]
fn service_start_waits_for_ready() {
    let f = service_fixture();
    let result = Resources::new(&f.ops())
        .service("local", f.host(), "mail", "start")
        .unwrap();
    assert_eq!(result["ready"], true);
    assert!(!f.runner.did("sudo"));
}
#[test]
fn failing_probe_does_not_prevent_stopping() {
    let f = service_fixture();
    *f.runner.probe_fails.lock().unwrap() = true;
    let result = Resources::new(&f.ops())
        .service("local", f.host(), "mail", "stop")
        .unwrap();
    assert_eq!(result["running"], false);
    assert!(!f.runner.did("/fake/probe"));
}
#[test]
fn readiness_timeout_is_not_success() {
    let f = service_fixture();
    *f.runner.probe_fails.lock().unwrap() = true;
    assert!(Resources::new(&f.ops())
        .service("local", f.host(), "mail", "start")
        .is_err());
}
#[test]
fn system_service_elevates_mutation_only() {
    let mut f = service_fixture();
    f.inventory
        .hosts
        .get_mut("local")
        .unwrap()
        .services
        .get_mut("mail")
        .unwrap()
        .scope = Scope::System;
    Resources::new(&f.ops())
        .service("local", f.host(), "mail", "stop")
        .unwrap();
    assert!(f.runner.did("sudo --"));
    assert!(!f
        .runner
        .commands()
        .iter()
        .any(|a| a[0] == "sudo" && (a.contains(&"print".into()) || a.contains(&"show".into()))));
}
#[test]
fn service_missing_model_prevents_start() {
    let mut f = model_fixture();
    let service = service_fixture().host().services["mail"].clone();
    f.inventory.hosts.get_mut("local").unwrap().services.insert(
        "mail".into(),
        Service {
            models: vec!["triage".into()],
            ..service
        },
    );
    assert!(Resources::new(&f.ops())
        .service("local", f.host(), "mail", "start")
        .is_err());
    assert!(f.runner.commands().is_empty());
}
#[test]
fn launchd_stop_requires_restart_policy() {
    if local_platform() != Platform::Darwin {
        return;
    }
    let mut f = service_fixture();
    f.inventory
        .hosts
        .get_mut("local")
        .unwrap()
        .services
        .get_mut("mail")
        .unwrap()
        .restart_on_exit = None;
    assert!(Resources::new(&f.ops())
        .service("local", f.host(), "mail", "stop")
        .is_err());
    assert!(!f.runner.did("kill"));
}
fn task_fixture(body: &str, timeout: f64) -> Fixture {
    let mut f = Fixture::new();
    let script = f.temp.path().join("task");
    write_script(&script, body);
    f.inventory.hosts.get_mut("local").unwrap().tasks.insert(
        "triage".into(),
        Task {
            argv: vec![script.to_string_lossy().into()],
            cwd: f.temp.path().into(),
            timeout,
            models: vec![],
            services: vec![],
            secrets: Default::default(),
            user: None,
        },
    );
    f
}
#[test]
fn foreground_task_has_private_output_and_history() {
    let f = task_fixture("printf 'proposal\\n%s\\n' \"$RIG_RUN_ID\"", 5.0);
    let ops = f.ops();
    let r = Resources::new(&ops);
    let result = r.task_run("local", f.host(), "triage").unwrap();
    assert_eq!(result["state"], "succeeded");
    let path = result["log"].as_str().unwrap();
    assert!(fs::read_to_string(path).unwrap().contains("proposal"));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let runs = r.task_runs("local", f.host(), "triage").unwrap();
    assert_eq!(runs[0]["id"], result["id"]);
    assert!(runs[0].get("argv").is_none());
}
#[test]
fn task_failure_is_recorded_without_retry() {
    let f = task_fixture("exit 7", 5.0);
    let error = Resources::new(&f.ops())
        .task_run("local", f.host(), "triage")
        .unwrap_err();
    assert_eq!(error.details["exitCode"], 7);
    assert_eq!(
        Resources::new(&f.ops())
            .task_runs("local", f.host(), "triage")
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn task_timeout_stops_term_resistant_descendants() {
    let f = task_fixture(
        "(trap '' TERM; while :; do printf x >> beat; sleep 0.02; done) & wait",
        1.0,
    );
    let error = Resources::new(&f.ops())
        .task_run("local", f.host(), "triage")
        .unwrap_err();
    assert_eq!(error.details["state"], "timed-out");
    let size = fs::metadata(f.temp.path().join("beat")).unwrap().len();
    assert!(size > 0, "descendant did not start before the deadline");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        fs::metadata(f.temp.path().join("beat")).unwrap().len(),
        size
    );
}
#[test]
fn overlapping_task_is_refused() {
    let f = task_fixture("exit 0", 5.0);
    let _lock = f.state.lock("task-local-triage").unwrap();
    assert!(Resources::new(&f.ops())
        .task_run("local", f.host(), "triage")
        .is_err());
    assert!(!f.state.root.join("runs").exists());
}
#[test]
fn unfinished_task_blocks_retry() {
    let f = task_fixture("exit 0", 5.0);
    f.state.write("runs/run-aaaaaaaaaaaaaaaa/record.json",&json!({"id":"run-aaaaaaaaaaaaaaaa","host":"local","task":"triage","state":"running","startedAt":"2026"})).unwrap();
    let ops = f.ops();
    let r = Resources::new(&ops);
    assert_eq!(
        r.task_runs("local", f.host(), "triage").unwrap()[0]["state"],
        "unknown"
    );
    assert!(r.task_run("local", f.host(), "triage").is_err());
}
#[test]
fn missing_secret_prevents_launch() {
    let mut f = task_fixture("exit 0", 5.0);
    f.inventory
        .hosts
        .get_mut("local")
        .unwrap()
        .tasks
        .get_mut("triage")
        .unwrap()
        .secrets
        .insert("mail".into(), f.temp.path().join("missing"));
    assert!(Resources::new(&f.ops())
        .task_run("local", f.host(), "triage")
        .is_err());
    assert!(!f.state.root.join("runs").exists());
}
#[test]
fn task_logs_reject_traversal() {
    let f = task_fixture("exit 0", 5.0);
    assert!(Resources::new(&f.ops())
        .task_logs("local", f.host(), "../../secret", false)
        .is_err());
}
/// Streams zeros until cancelled and delivers SIGINT on its third read, so
/// only cancellation can end the digest.
struct CancelsMidStream {
    reads: usize,
}
impl Read for CancelsMidStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        assert!(self.reads < 1000, "hashing ignored cancellation");
        if self.reads == 3 {
            cancel_with(libc::SIGINT);
        }
        buffer.fill(0);
        Ok(buffer.len())
    }
}
#[test]
fn hashing_stops_mid_stream_when_cancelled() {
    if !isolated("hashing_stops_mid_stream_when_cancelled") {
        return;
    }
    let mut reader = CancelsMidStream { reads: 0 };
    let error = hash_reader(&mut reader).unwrap_err();
    assert!(error.message.contains("interrupted"), "{}", error.message);
    assert_eq!(reader.reads, 3, "hashing continued after cancellation");
}
fn launchable_task(models: &[&str]) -> Fixture {
    let mut f = model_fixture();
    let script = f.temp.path().join("task");
    write_script(&script, "printf x >> started");
    f.inventory.hosts.get_mut("local").unwrap().tasks.insert(
        "triage".into(),
        Task {
            argv: vec![script.to_string_lossy().into()],
            cwd: f.temp.path().into(),
            timeout: 5.0,
            models: models.iter().map(ToString::to_string).collect(),
            services: vec![],
            secrets: Default::default(),
            user: None,
        },
    );
    f
}
#[test]
fn task_never_starts_once_cancelled() {
    if !isolated("task_never_starts_once_cancelled") {
        return;
    }
    let hashed = launchable_task(&["triage"]);
    Resources::new(&hashed.ops())
        .fetch("local", hashed.host(), "triage")
        .unwrap();
    let direct = launchable_task(&[]);
    cancel_with(libc::SIGINT);
    // One task is cancelled while verifying its model, the other at launch.
    for f in [&hashed, &direct] {
        let error = Resources::new(&f.ops())
            .task_run("local", f.host(), "triage")
            .unwrap_err();
        assert!(error.message.contains("interrupted"), "{}", error.message);
        assert!(
            !f.temp.path().join("started").exists(),
            "the task started after cancellation"
        );
        assert!(
            !f.state.root.join("runs").exists(),
            "a cancelled launch must leave no run record"
        );
    }
}
/// Backend whose service mutations hang in a real process, so the native
/// runner's own deadline is what ends them.
struct HangingMutation<'a> {
    backend: &'a FakeRunner,
    hang: PathBuf,
}
impl Runner for HangingMutation<'_> {
    fn run(&self, mut command: Invocation) -> Result<Output> {
        if command.stream == Stream::Progress {
            command.argv = vec![self.hang.to_string_lossy().into_owned()];
            return NativeRunner.run(command);
        }
        self.backend.run(command)
    }
    fn active_system(&self, path: &Path) -> Result<Option<String>> {
        self.backend.active_system(path)
    }
}
#[test]
fn hung_service_mutation_ends_at_the_declared_deadline() {
    let mut f = service_fixture();
    f.inventory
        .hosts
        .get_mut("local")
        .unwrap()
        .services
        .get_mut("mail")
        .unwrap()
        .timeout = 0.4;
    let hang = f.temp.path().join("hang");
    write_script(&hang, "exec sleep 30");
    let backend = HangingMutation {
        backend: &f.runner,
        hang,
    };
    let mut ops = f.ops();
    ops.runner = &backend;
    let start = Instant::now();
    let error = Resources::new(&ops)
        .service("local", f.host(), "mail", "start")
        .unwrap_err();
    let elapsed = start.elapsed();
    assert!(error.message.contains("timed out"), "{}", error.message);
    assert!(
        elapsed >= Duration::from_millis(300),
        "the declared timeout was truncated: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the 60 second default applied instead of the declared timeout: {elapsed:?}"
    );
}
fn cli_inventory(tasks: Value, services: Value) -> Value {
    let mut hosts = serde_json::Map::new();
    hosts.insert(
        local_name(),
        json!({"platform": local_platform(), "managed": true, "tasks": tasks, "services": services}),
    );
    json!({"version": 1, "hosts": hosts})
}
#[test]
fn hangup_cancels_task_and_stops_every_descendant() {
    let f = Fixture::new();
    let work = f.temp.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = f.temp.path().join("task");
    // The descendant ignores TERM and is bounded, so a failure cannot leak it for long.
    write_script(
        &script,
        "echo $$ > pid\n(trap '' TERM; n=0; while [ \"$n\" -lt 3000 ]; do printf x >> beat; n=$((n+1)); sleep 0.02; done) &\nwait",
    );
    let inventory = cli_inventory(
        json!({"triage": {"argv": [script], "cwd": work, "timeout": 120}}),
        json!({}),
    );
    let mut command = rig_command(&f, inventory);
    let name = local_name();
    command.args(["task", "run", "triage", "--on", name.as_str(), "--json"]);
    let child = spawn(command);
    let beat = work.join("beat");
    wait_until(
        "the task's descendant to start",
        Duration::from_secs(60),
        || fs::metadata(&beat).is_ok_and(|m| m.len() > 0),
    );
    signal(&child, libc::SIGHUP);
    let output = finish(child, Duration::from_secs(30));
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["results"][0]["details"]["state"], "cancelled");
    let group: i32 = fs::read_to_string(work.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_until(
        "the task's process group to disappear",
        Duration::from_secs(10),
        || unsafe { libc::kill(-group, 0) } != 0,
    );
    let runs: Vec<_> = fs::read_dir(f.temp.path().join("state-home/rig/runs"))
        .unwrap()
        .collect();
    assert_eq!(runs.len(), 1);
    let record: Value = serde_json::from_slice(
        &fs::read(runs[0].as_ref().unwrap().path().join("record.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["state"], "cancelled");
}
#[test]
fn cancel_during_service_preflight_never_launches_task() {
    let f = Fixture::new();
    let work = f.temp.path().join("work");
    fs::create_dir(&work).unwrap();
    let task = f.temp.path().join("task");
    write_script(&task, "printf x >> started");
    let probe = f.temp.path().join("probe");
    write_script(&probe, ": > \"$RIG_FAKE_PROBING\"\nexec sleep 30");
    let manager = if local_platform() == Platform::Darwin {
        "launchd"
    } else {
        "systemd"
    };
    let inventory = cli_inventory(
        json!({"triage": {"argv": [task], "cwd": work, "services": ["mail"]}}),
        json!({"mail": {"manager": manager, "scope": "user", "unit": "dev.mail", "ready": [probe]}}),
    );
    let mut command = rig_command(&f, inventory);
    let bin = f.temp.path().join("bin");
    write_script(
        &bin.join("systemctl"),
        "printf 'LoadState=loaded\\nActiveState=active\\nSubState=running\\n'",
    );
    write_script(&bin.join("launchctl"), "printf 'state = running\\n'");
    let probing = f.temp.path().join("probing");
    let name = local_name();
    command
        .args(["task", "run", "triage", "--on", name.as_str(), "--json"])
        .env("RIG_FAKE_PROBING", &probing);
    let child = spawn(command);
    wait_until(
        "the readiness probe to start",
        Duration::from_secs(60),
        || probing.exists(),
    );
    signal(&child, libc::SIGINT);
    let output = finish(child, Duration::from_secs(30));
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = result["results"][0]["error"].as_str().unwrap();
    assert!(
        message.contains("interrupted"),
        "cancellation was reported as: {message}"
    );
    assert!(!work.join("started").exists(), "the task started");
    assert!(!f.temp.path().join("state-home/rig/runs").exists());
}
