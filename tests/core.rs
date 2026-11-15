mod common;
use clap::Parser;
use common::*;
use rig::{cli::Cli, core::*, inventory::*};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    time::Duration,
};

#[test]
fn inventory_requires_explicit_ownership() {
    let mut data = inventory();
    data["hosts"]["local"]
        .as_object_mut()
        .unwrap()
        .remove("managed");
    assert!(Inventory::parse(data).is_err());
}
#[test]
fn inventory_rejects_unknown_fields() {
    let mut data = inventory();
    data["hosts"]["local"]["automaticPrune"] = json!(true);
    assert!(Inventory::parse(data).is_err());
}
#[test]
fn inventory_version_is_strict() {
    for v in [json!(true), json!(2), json!("1"), json!(null)] {
        let mut d = inventory();
        d["version"] = v;
        assert!(Inventory::parse(d).is_err());
    }
}
#[test]
fn inventory_rejects_shellish_ssh() {
    for host in [
        "-oProxyCommand=bad",
        "host;command",
        "user@host",
        "two hosts",
    ] {
        let mut d = inventory();
        d["hosts"]["remote"]["ssh"]["host"] = json!(host);
        assert!(Inventory::parse(d).is_err());
    }
}
#[test]
fn inventory_requires_task_absolute_executable() {
    let mut d = inventory();
    d["hosts"]["local"]["tasks"] = json!({"mail":{"argv":["sh","-c","x"],"cwd":"/tmp"}});
    assert!(Inventory::parse(d).is_err());
}
#[test]
fn inventory_rejects_unknown_task_dependencies() {
    let mut d = inventory();
    d["hosts"]["local"]["tasks"] =
        json!({"mail":{"argv":["/bin/true"],"cwd":"/tmp","models":["missing"]}});
    assert!(Inventory::parse(d).is_err());
}
#[test]
fn model_hash_must_be_pinned() {
    let mut d = inventory();
    d["hosts"]["local"]["models"] = json!({"m":{"sha256":"latest"}});
    assert!(Inventory::parse(d).is_err());
}
#[test]
fn selection_excludes_delegated_hosts() {
    let inv = Inventory::parse(inventory()).unwrap();
    let local = BTreeSet::from(["local".into()]);
    assert_eq!(inv.select(None, true, &local, true).unwrap().len(), 2);
    assert!(inv.select(Some("work"), false, &local, true).is_err());
    assert_eq!(inv.select(None, false, &local, true).unwrap()[0].0, "local");
}
#[test]
fn unknown_local_name_requires_target() {
    let inv = Inventory::parse(inventory()).unwrap();
    assert!(inv.select(None, false, &BTreeSet::new(), true).is_err());
}
#[test]
fn store_paths_are_validated() {
    assert!(store_path(CLOSURE).is_ok());
    for path in [
        "/etc/passwd",
        "-bad",
        "/nix/store/a-system",
        &format!("{CLOSURE}/bin/tool"),
    ] {
        assert!(store_path(path).is_err());
    }
}
#[test]
fn state_is_private_and_atomic() {
    let f = Fixture::new();
    f.state.write("hosts/test.json", &json!({"x":1})).unwrap();
    assert_eq!(f.state.read("hosts/test.json").unwrap().unwrap()["x"], 1);
    for p in [f.state.root.clone(), f.state.root.join("hosts")] {
        assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o700);
    }
    assert_eq!(
        fs::metadata(f.state.root.join("hosts/test.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn locks_reject_overlap_and_release() {
    let f = Fixture::new();
    let lock = f.state.lock("system").unwrap();
    assert!(f.state.lock("system").is_err());
    drop(lock);
    assert!(f.state.lock("system").is_ok());
}
#[test]
fn locks_do_not_follow_symlinks() {
    let f = Fixture::new();
    let dir = f.state.dir("locks").unwrap();
    symlink(f.temp.path().join("victim"), dir.join("system")).unwrap();
    assert!(f.state.lock("system").is_err());
    assert!(!f.temp.path().join("victim").exists());
}
#[test]
fn corrupt_state_is_not_ignored() {
    let f = Fixture::new();
    f.state.dir("").unwrap();
    fs::write(f.state.root.join("bad.json"), "bad").unwrap();
    assert!(f.state.read("bad.json").is_err());
}
#[test]
fn state_rejects_relative_root() {
    assert!(State::new("relative".into()).is_err());
}
#[test]
fn state_rejects_nested_symlink() {
    let f = Fixture::new();
    f.state.dir("").unwrap();
    let victim = f.temp.path().join("victim");
    fs::create_dir(&victim).unwrap();
    symlink(&victim, f.state.root.join("runs")).unwrap();
    assert!(f.state.dir("runs/id").is_err());
    assert!(!victim.join("id").exists());
}
#[test]
fn native_errors_do_not_reveal_stderr() {
    let output = NativeRunner
        .run(Invocation::new([
            shell().to_string_lossy().as_ref(),
            "-c",
            "echo SECRET >&2; exit 8",
        ]))
        .unwrap_err();
    assert!(!output.message.contains("SECRET"));
    assert_eq!(output.code, 1);
}
#[test]
fn missing_executable_is_a_refusal() {
    let error = NativeRunner
        .run(Invocation::new(["/no/rig-command"]))
        .unwrap_err();
    assert_eq!(error.code, 2);
}
#[test]
fn captured_process_timeout_is_bounded() {
    let mut spec = Invocation::new([shell().to_string_lossy().as_ref(), "-c", "sleep 30"]);
    spec.timeout = Some(Duration::from_millis(30));
    let start = std::time::Instant::now();
    assert!(NativeRunner.run(spec).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn timeout_kills_a_term_resistant_descendant() {
    let temp = tempfile::tempdir().unwrap();
    let beat = temp.path().join("beat");
    let script = temp.path().join("worker");
    write_script(
        &script,
        "(trap '' TERM; while :; do printf x >> \"$1\"; sleep 0.02; done) & wait",
    );
    let mut command = Invocation::new([
        script.to_string_lossy().into_owned(),
        beat.to_string_lossy().into_owned(),
    ]);
    command.timeout = Some(Duration::from_secs(1));
    assert!(NativeRunner.run(command).is_err());
    let length = fs::metadata(&beat).unwrap().len();
    assert!(length > 0, "descendant did not start before the deadline");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::metadata(beat).unwrap().len(), length);
}
#[test]
fn headless_streamed_timeout_kills_descendants() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let beat = temp.path().join("beat");
    let script = temp.path().join("worker");
    write_script(
        &script,
        "(trap '' TERM; while :; do printf x >> \"$1\"; sleep 0.02; done) & wait",
    );
    let mut command = Invocation::new([
        script.to_string_lossy().into_owned(),
        beat.to_string_lossy().into_owned(),
    ])
    .stream(Stream::Progress);
    command.timeout = Some(Duration::from_secs(1));
    assert!(NativeRunner.run(command).is_err());
    let length = fs::metadata(&beat).unwrap().len();
    assert!(length > 0, "descendant did not start before the deadline");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::metadata(beat).unwrap().len(), length);
}
#[test]
fn global_flags_work_at_each_level() {
    for args in [
        vec!["rig", "--json", "host", "list"],
        vec!["rig", "host", "--json", "list"],
        vec!["rig", "host", "list", "--json"],
    ] {
        assert!(Cli::try_parse_from(args).unwrap().json);
    }
}
#[test]
fn repository_commands_reject_host_target() {
    assert!(Cli::try_parse_from(["rig", "bump", "--on", "local"]).is_err());
}
#[test]
fn target_flags_are_mutually_exclusive() {
    assert!(Cli::try_parse_from(["rig", "status", "--on", "local", "--all"]).is_err());
}
#[test]
fn flake_path_is_explicit() {
    let f = Fixture::new();
    assert_eq!(
        flake_path(Some(&f.flake)).unwrap(),
        f.flake.canonicalize().unwrap()
    );
    assert!(flake_path(Some(&f.temp.path().join("missing"))).is_err());
}
