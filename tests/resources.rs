mod common;
use common::*;
use rig::{inventory::*, resources::*};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{symlink, PermissionsExt},
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
