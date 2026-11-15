mod common;
use common::*;
use serde_json::{json, Value};
use std::{
    fs,
    process::{Command, Output},
};
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rig"))
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn help_and_version() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("bump"));
    assert!(run(&["--version"]).status.success());
}
#[test]
fn invalid_arguments_produce_json() {
    let out = run(&["--json", "not-a-command"]);
    assert_eq!(out.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert!(out.stderr.is_empty());
}
#[test]
fn unavailable_operations_fail_before_loading_flake() {
    for args in [
        vec!["rollback"],
        vec!["secret", "rekey"],
        vec!["model", "prune", "--apply"],
        vec!["task", "run", "mail", "--detach"],
        vec!["host", "enroll", "x", "--address", "x"],
    ] {
        let mut args = args;
        args.push("--json");
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2));
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("not implemented"));
    }
}
#[test]
fn logs_reject_json() {
    let out = run(&["service", "logs", "mail", "--json"]);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(result["error"].as_str().unwrap().contains("log streams"));
}
#[test]
fn current_directory_is_not_an_implicit_flake() {
    let f = Fixture::new();
    let out = Command::new(env!("CARGO_BIN_EXE_rig"))
        .args(["host", "list", "--json"])
        .current_dir(&f.flake)
        .env_remove("RIG_FLAKE")
        .env_remove("NH_FLAKE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result["error"].as_str().unwrap().contains("select --flake"));
}
fn fake_cli(f: &Fixture, args: &[&str], data: Value) -> Output {
    rig_command(f, data).args(args).output().unwrap()
}
#[test]
fn binary_queries_inventory_with_versioned_json() {
    let f = Fixture::new();
    let out = fake_cli(&f, &["host", "list", "--json"], inventory());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["version"], 1);
    assert_eq!(result["results"].as_array().unwrap().len(), 3);
    assert!(fs::read_to_string(f.temp.path().join("calls"))
        .unwrap()
        .contains("allow-import-from-derivation false"));
}
#[test]
fn binary_rejects_unknown_inventory_fields() {
    let f = Fixture::new();
    let mut data = inventory();
    data["hosts"]["local"]["unsafePolicy"] = json!(true);
    let out = fake_cli(&f, &["host", "list", "--json"], data);
    assert_eq!(out.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result["error"].as_str().unwrap().contains("unknown field"));
}
#[test]
fn binary_remote_switch_is_rejected_before_build() {
    let f = Fixture::new();
    let out = fake_cli(
        &f,
        &["switch", "--on", "remote", "--yes", "--json"],
        inventory(),
    );
    assert_eq!(out.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("remote mutation"));
    assert_eq!(
        fs::read_to_string(f.temp.path().join("calls"))
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("CALL "))
            .count(),
        3
    );
}
#[test]
fn binary_fleet_preflight_does_not_partially_build() {
    let f = Fixture::new();
    let out = fake_cli(&f, &["build", "--all", "--json"], inventory());
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(f.temp.path().join("calls"))
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("CALL "))
            .count(),
        3
    );
}
