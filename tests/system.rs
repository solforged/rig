mod common;
use common::*;
use rig::{core::*, inventory::*};
use serde_json::json;

#[test]
fn build_uses_frozen_source_and_roots_output() {
    let f = Fixture::new();
    let record = f.ops().build("local", f.host()).unwrap();
    assert_eq!(record["closure"], CLOSURE);
    let commands = f.runner.commands();
    let build = commands
        .iter()
        .find(|a| a.starts_with(&["nix".into(), "build".into()]))
        .unwrap();
    assert!(build.last().unwrap().starts_with(&format!("{SOURCE}#")));
    assert!(build.contains(&"--out-link".into()));
    assert!(build.contains(&"--no-update-lock-file".into()));
    assert!(f.runner.did("nix-store --add-root"));
    assert!(!f.runner.did("nh"));
}
#[test]
fn inventory_race_prevents_build() {
    let f = Fixture::new();
    f.runner.data.lock().unwrap()["hosts"]["local"]["managed"] = json!(false);
    f.runner.data.lock().unwrap()["hosts"]["local"]["owner"] = json!("other");
    assert!(f.ops().build("local", f.host()).is_err());
    assert!(!f.runner.did("nix build"));
}
#[test]
fn failed_build_never_activates() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    *f.runner.fail.lock().unwrap() = Some("nix build".into());
    assert!(f.ops().switch("local", f.host(), false).is_err());
    assert!(!f
        .runner
        .commands()
        .iter()
        .any(|a| a[0] == "nh" && !a.contains(&"--help".into())));
}
#[test]
fn switch_activates_exact_closure() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    let record = f.ops().switch("local", f.host(), false).unwrap();
    assert_eq!(record["active"], CLOSURE);
    assert_eq!(record["previous"], OLD);
    let calls = f.runner.commands();
    let activation = calls
        .iter()
        .find(|a| a[0] == "nh" && !a.contains(&"--help".into()))
        .unwrap();
    if f.host().platform == rig::inventory::Platform::Darwin {
        let expression = activation
            .windows(2)
            .find(|args| args[0] == "--expr")
            .unwrap();
        assert!(expression[1].contains(CLOSURE));
        assert!(activation.iter().any(|arg| arg == "--impure"));
    } else {
        assert_eq!(activation[3], CLOSURE);
    }
    assert!(!activation.contains(&f.flake.to_string_lossy().into()));
}
#[test]
fn failed_activation_is_visible_in_status() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    *f.runner.switch_fails.lock().unwrap() = true;
    assert!(f.ops().switch("local", f.host(), false).is_err());
    let status = f.ops().status("local", f.host()).unwrap();
    assert_eq!(status["lastActivationAttempt"]["state"], "unknown");
    assert_eq!(status["lastActivationAttempt"]["observedActive"], CLOSURE);
    assert_eq!(status["provenance"], "unknown");
}
#[test]
fn unresolved_activation_blocks_reentry_before_build() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    f.state
        .write("hosts/local.json", &json!({"state":"unknown"}))
        .unwrap();
    let error = f.ops().switch("local", f.host(), false).unwrap_err();
    assert!(error.message.contains("unresolved"));
    assert!(!f.runner.did("nix build"));
    assert!(!f
        .runner
        .commands()
        .iter()
        .any(|a| a[0] == "nh" && !a.contains(&"--help".into())));
}
#[test]
fn wrong_active_closure_is_not_success() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    *f.runner.after_switch.lock().unwrap() = OLD.into();
    assert!(f.ops().switch("local", f.host(), false).is_err());
    assert_eq!(
        f.state.read("hosts/local.json").unwrap().unwrap()["state"],
        "unknown"
    );
}
#[test]
fn dry_run_does_not_build_or_freeze() {
    if uid() == 0 {
        return;
    }
    let f = Fixture::new();
    let result = f.ops().switch("local", f.host(), true).unwrap();
    assert_eq!(result["dryRun"], true);
    assert_eq!(f.runner.commands().len(), 1);
    assert!(f.runner.did("--help"));
}
#[test]
fn remote_mutation_stops_before_native_commands() {
    let f = Fixture::new();
    assert!(f
        .ops()
        .build("remote", &f.inventory.hosts["remote"])
        .is_err());
    assert!(f.runner.commands().is_empty());
}
#[test]
fn delegated_host_cannot_mutate() {
    let f = Fixture::new();
    assert!(f.ops().build("work", &f.inventory.hosts["work"]).is_err());
    assert!(f.runner.commands().is_empty());
}
#[test]
fn ssh_inspection_is_strict_and_bounded() {
    let f = Fixture::new();
    let result = f
        .ops()
        .status("remote", &f.inventory.hosts["remote"])
        .unwrap();
    assert_eq!(result["reachability"], "reachable");
    let calls = f.runner.calls.lock().unwrap();
    let call = &calls[0];
    assert!(call.argv.contains(&"StrictHostKeyChecking=yes".into()));
    assert!(call.argv.contains(&"BatchMode=yes".into()));
    assert!(call.argv.contains(&"-a".into()));
    assert_eq!(call.timeout.unwrap().as_secs(), 20);
    assert_eq!(result["desired"], "unknown");
}
#[test]
fn unreachable_is_not_healthy() {
    let f = Fixture::new();
    *f.runner.fail.lock().unwrap() = Some("ssh".into());
    let result = f
        .ops()
        .status("remote", &f.inventory.hosts["remote"])
        .unwrap();
    assert_eq!(result["reachability"], "unreachable");
    assert_eq!(result["code"], 1);
}
#[test]
fn status_does_not_trust_mismatched_record() {
    let f = Fixture::new();
    f.state
        .write(
            "hosts/local.json",
            &json!({"state":"activated","active":CLOSURE}),
        )
        .unwrap();
    let result = f.ops().status("local", f.host()).unwrap();
    assert_eq!(result["provenance"], "unknown");
    assert!(result.get("lastActivationAttempt").is_some());
}
#[test]
fn gc_preview_never_deletes() {
    let f = Fixture::new();
    let result = f.ops().gc("local", f.host(), false).unwrap();
    assert_eq!(result["preview"], true);
    assert_eq!(result["generationPruning"], false);
    assert_eq!(
        f.runner.commands(),
        vec![vec!["nix-store", "--gc", "--print-dead"]]
    );
}
#[test]
fn gc_apply_is_platform_specific_and_never_prunes_profiles() {
    let f = Fixture::new();
    let result = f.ops().gc("local", f.host(), true);
    if local_platform() == Platform::Darwin {
        assert!(result.is_err());
        assert!(f.runner.commands().is_empty());
    } else {
        assert!(result.is_ok());
        assert_eq!(
            f.runner.commands().last().unwrap(),
            &vec!["nix-store", "--gc"]
        );
        assert!(!f.runner.did("--delete"));
        assert!(!f.runner.did("nix-env"));
    }
}
#[test]
fn successful_build_provenance_becomes_active_only_when_observed() {
    let f = Fixture::new();
    f.ops().build("local", f.host()).unwrap();
    assert_eq!(
        f.ops().status("local", f.host()).unwrap()["desired"],
        "built-not-active"
    );
    *f.runner.active.lock().unwrap() = Some(CLOSURE.into());
    assert_eq!(
        f.ops().status("local", f.host()).unwrap()["desired"],
        "active"
    );
}
