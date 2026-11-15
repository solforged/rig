mod common;
use common::*;
use rig::{
    bump::{BumpOptions, Bumper},
    core::*,
    system::find_executable,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

struct Repo {
    fixture: Fixture,
    root: PathBuf,
    original: Vec<u8>,
}
impl Repo {
    fn new() -> Option<Self> {
        if find_executable("jj").is_none() || find_executable("git").is_none() {
            eprintln!("skipping jj fixture: jj and Git required");
            return None;
        }
        let mut f = Fixture::new();
        f.inventory.hosts.remove("remote");
        *f.runner.data.lock().unwrap() = serde_json::to_value(&f.inventory).unwrap();
        let config = f.temp.path().join("jj.toml");
        fs::write(&config,"[user]\nname = \"Rig Test\"\nemail = \"rig@example.invalid\"\n[signing]\nbehavior = \"drop\"\n").unwrap();
        f.runner.jj_config = Some(config);
        f.runner
            .run(Invocation::new([
                "jj",
                "git",
                "init",
                "--colocate",
                &f.flake.to_string_lossy(),
            ]))
            .unwrap();
        let lock=json!({"version":7,"root":"root","nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs"}},"nixpkgs":{"locked":{"rev":"old","narHash":"sha256-old"}}}}).to_string();
        fs::write(f.flake.join("flake.lock"), &lock).unwrap();
        fs::write(f.flake.join("notes.txt"), "original\n").unwrap();
        f.runner
            .run(
                Invocation::new(["jj", "describe", "-m", "test: added flake fixture"])
                    .cwd(&f.flake),
            )
            .unwrap();
        f.runner
            .run(Invocation::new(["jj", "new"]).cwd(&f.flake))
            .unwrap();
        let root = f.temp.path().join("workspaces");
        Some(Self {
            fixture: f,
            root,
            original: lock.into_bytes(),
        })
    }
    fn jj(&self, args: &[&str]) -> String {
        let mut a = vec!["jj", "--no-pager"];
        a.extend(args);
        self.fixture
            .runner
            .run(Invocation::new(a).cwd(&self.fixture.flake))
            .unwrap()
            .stdout
            .trim()
            .into()
    }
    fn run(&self, no_commit: bool, eval_only: bool) -> Result<serde_json::Value> {
        let ops = self.fixture.ops();
        Bumper {
            ops: &ops,
            workspace_root: self.root.clone(),
        }
        .run(&BumpOptions {
            inputs: vec![],
            hosts: vec![],
            no_commit,
            eval_only,
            message: None,
        })
    }
    fn clean(&self) {
        self.clean_in(&self.root);
    }
    fn clean_in(&self, root: &Path) {
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
        assert!(!self.jj(&["workspace", "list"]).contains("rig-bump-"));
        let out = NativeRunner
            .run(
                Invocation::new(["git", "worktree", "list", "--porcelain"])
                    .cwd(&self.fixture.flake),
            )
            .unwrap();
        assert!(!out.stdout.contains("rig-bump-"));
    }
}
#[test]
fn commit_inserts_only_lock_and_preserves_unfinished_work() {
    let Some(r) = Repo::new() else {
        return;
    };
    fs::write(r.fixture.flake.join("notes.txt"), "unfinished\n").unwrap();
    let change = r.jj(&["log", "--no-graph", "-r", "@", "-T", "change_id"]);
    let result = r.run(false, true).unwrap();
    assert_eq!(result["committed"], true);
    assert_eq!(r.jj(&["diff", "--name-only", "-r", "@-"]), "flake.lock");
    assert_eq!(r.jj(&["diff", "--name-only", "-r", "@"]), "notes.txt");
    assert_eq!(
        r.jj(&["log", "--no-graph", "-r", "@", "-T", "change_id"]),
        change
    );
    assert!(r
        .jj(&["log", "--no-graph", "-r", "@-", "-T", "description"])
        .contains("chore(flake): bumped nixpkgs"));
    r.clean();
}
#[test]
fn no_commit_retains_candidate_without_touching_checkout() {
    let Some(r) = Repo::new() else {
        return;
    };
    fs::write(r.fixture.flake.join("notes.txt"), "unfinished\n").unwrap();
    let result = r.run(true, true).unwrap();
    assert_eq!(result["committed"], false);
    assert_eq!(result["applied"], false);
    assert_eq!(
        fs::read(r.fixture.flake.join("flake.lock")).unwrap(),
        r.original
    );
    let path = PathBuf::from(result["candidateWorkspace"].as_str().unwrap());
    assert!(path.join("flake.lock").is_file());
    assert_eq!(r.jj(&["diff", "--name-only", "-r", "@"]), "notes.txt");
}
#[test]
fn review_mode_never_overwrites_concurrent_lock_saves() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.concurrent_edit.lock().unwrap() =
        Some((r.fixture.flake.join("flake.lock"), "saved by editor".into()));
    let result = r.run(true, true).unwrap();
    assert_eq!(
        fs::read_to_string(r.fixture.flake.join("flake.lock")).unwrap(),
        "saved by editor"
    );
    assert!(result["candidateWorkspace"].is_string());
}
#[test]
fn failed_validation_leaves_original_untouched() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.fail.lock().unwrap() = Some("nix eval --raw".into());
    fs::write(r.fixture.flake.join("notes.txt"), "unfinished").unwrap();
    assert!(r.run(false, true).is_err());
    assert_eq!(
        fs::read(r.fixture.flake.join("flake.lock")).unwrap(),
        r.original
    );
    assert_eq!(
        fs::read_to_string(r.fixture.flake.join("notes.txt")).unwrap(),
        "unfinished"
    );
    r.clean();
}
#[test]
fn existing_lock_edit_is_refused() {
    let Some(r) = Repo::new() else {
        return;
    };
    fs::write(r.fixture.flake.join("flake.lock"), "changed").unwrap();
    let error = r.run(false, true).unwrap_err();
    assert!(error.message.contains("uncommitted edits"));
    assert!(!r.root.exists());
}
#[test]
fn noop_does_not_commit() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.noop.lock().unwrap() = true;
    let parent = r.jj(&["log", "--no-graph", "-r", "@-", "-T", "commit_id"]);
    assert_eq!(r.run(false, true).unwrap()["changed"], false);
    assert_eq!(
        r.jj(&["log", "--no-graph", "-r", "@-", "-T", "commit_id"]),
        parent
    );
    r.clean();
}
#[test]
fn concurrent_edit_retains_candidate_and_does_not_commit() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.concurrent_edit.lock().unwrap() =
        Some((r.fixture.flake.join("notes.txt"), "concurrent".into()));
    let error = r.run(false, true).unwrap_err();
    assert!(error.details["candidateWorkspace"].is_string());
    assert_eq!(
        fs::read(r.fixture.flake.join("flake.lock")).unwrap(),
        r.original
    );
    assert_eq!(
        fs::read_to_string(r.fixture.flake.join("notes.txt")).unwrap(),
        "concurrent"
    );
}
#[test]
fn default_bump_builds_local_host() {
    let Some(r) = Repo::new() else {
        return;
    };
    let result = r.run(false, false).unwrap();
    assert_eq!(result["builtHosts"], json!(["local"]));
    assert!(r.fixture.runner.did("nix build"));
    r.clean();
}
#[test]
fn bump_inputs_cannot_become_native_nix_options() {
    let Some(r) = Repo::new() else {
        return;
    };
    let ops = r.fixture.ops();
    let error = Bumper {
        ops: &ops,
        workspace_root: r.root.clone(),
    }
    .run(&BumpOptions {
        inputs: vec!["--output-lock-file".into(), "/outside/flake.lock".into()],
        hosts: vec![],
        eval_only: true,
        no_commit: false,
        message: None,
    })
    .unwrap_err();
    assert!(error.message.contains("invalid input name"));
    assert!(!r.root.exists());
    assert!(!r.fixture.runner.did("nix flake update"));
}
#[test]
fn failed_build_does_not_apply_lock() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.fail.lock().unwrap() = Some("nix build".into());
    assert!(r.run(false, false).is_err());
    assert_eq!(
        fs::read(r.fixture.flake.join("flake.lock")).unwrap(),
        r.original
    );
    r.clean();
}
/// Backend whose `jj workspace forget` always fails.
struct FailingForget<'a> {
    backend: &'a FakeRunner,
}
impl Runner for FailingForget<'_> {
    fn run(&self, command: Invocation) -> Result<Output> {
        if command.argv.iter().any(|a| a == "forget") {
            return Err(Error::runtime("jj exited with status 1"));
        }
        self.backend.run(command)
    }
    fn active_system(&self, path: &Path) -> Result<Option<String>> {
        self.backend.active_system(path)
    }
}
#[test]
fn failed_forget_does_not_strand_the_workspace_directory() {
    let Some(r) = Repo::new() else {
        return;
    };
    *r.fixture.runner.fail.lock().unwrap() = Some("nix eval --raw".into());
    let runner = FailingForget {
        backend: &r.fixture.runner,
    };
    let mut ops = r.fixture.ops();
    ops.runner = &runner;
    let result = Bumper {
        ops: &ops,
        workspace_root: r.root.clone(),
    }
    .run(&BumpOptions {
        inputs: vec![],
        hosts: vec![],
        eval_only: true,
        no_commit: false,
        message: None,
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read_dir(&r.root).unwrap().count(),
        0,
        "the directory must be removed even when forget fails"
    );
}
#[test]
fn cancelled_bump_forgets_and_removes_its_workspace() {
    let Some(r) = Repo::new() else {
        return;
    };
    let f = &r.fixture;
    let update = f.temp.path().join("hang-update");
    write_script(&update, ": > \"$RIG_FAKE_MARKER\"\nexec sleep 30");
    let marker = f.temp.path().join("updating");
    let mut command = rig_command(f, serde_json::to_value(&f.inventory).unwrap());
    command
        .args(["bump", "--eval-only", "--no-commit", "--json"])
        .env("RIG_FAKE_UPDATE", &update)
        .env("RIG_FAKE_MARKER", &marker)
        .env("JJ_CONFIG", f.runner.jj_config.as_ref().unwrap());
    let child = spawn(command);
    // The marker proves the workspace exists and `nix flake update` is running in it.
    wait_until(
        "nix flake update to start in the workspace",
        Duration::from_secs(60),
        || marker.exists(),
    );
    let root = f.temp.path().join("home/.pi/wt");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    signal(&child, libc::SIGINT);
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    r.clean_in(&root);
    assert_eq!(
        fs::read(f.flake.join("flake.lock")).unwrap(),
        r.original,
        "a cancelled bump must not touch the checkout"
    );
}
