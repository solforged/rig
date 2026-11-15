//! Isolated jj updates. Review mode never writes the operator's lockfile.
use crate::{core::*, inventory::*, system::Operations};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CHECKS_EXPR: &str = "f: builtins.mapAttrs (_: outputs: builtins.mapAttrs (_: drvs: builtins.mapAttrs (_: drv: drv.drvPath) drvs) outputs) { checks = f.checks or {}; packages = f.packages or {}; }";
pub struct BumpOptions {
    pub inputs: Vec<String>,
    pub hosts: Vec<String>,
    pub eval_only: bool,
    pub no_commit: bool,
    pub message: Option<String>,
}
pub struct Bumper<'a, 'b> {
    pub ops: &'a Operations<'b>,
    pub workspace_root: PathBuf,
}
impl<'a, 'b> Bumper<'a, 'b> {
    fn jj(&self, args: &[&str], cwd: &Path) -> Result<String> {
        let mut argv = vec!["jj", "--no-pager"];
        argv.extend(args);
        Ok(self
            .ops
            .runner
            .run(Invocation::new(argv).cwd(cwd))?
            .stdout
            .trim()
            .into())
    }
    fn revision(&self, path: &Path) -> Result<String> {
        self.jj(&["log", "--no-graph", "-r", "@", "-T", "commit_id"], path)
    }
    pub fn run(&self, options: &BumpOptions) -> Result<Value> {
        require(
            options
                .inputs
                .iter()
                .all(|name| !name.is_empty() && !name.starts_with('-') && !name.contains('\0')),
            "invalid input name: inputs cannot be empty or native Nix options",
        )?;
        let flake = &self.ops.flake;
        require(
            flake.join(".jj").exists(),
            "bump requires a jj checkout rooted at the flake",
        )?;
        require(
            flake.join("flake.lock").is_file(),
            "bump requires an existing lockfile",
        )?;
        require(
            !(options.eval_only && !options.hosts.is_empty()),
            "--eval-only and --check-host are mutually exclusive",
        )?;
        let mut selected = Vec::new();
        if !options.eval_only {
            if options.hosts.is_empty() {
                selected = self
                    .ops
                    .inventory
                    .select(None, false, &self.ops.names, true)?;
            } else {
                for host in &options.hosts {
                    if !selected.iter().any(|(n, _)| n == host) {
                        selected.extend(self.ops.inventory.select(
                            Some(host),
                            false,
                            &self.ops.names,
                            true,
                        )?);
                    }
                }
            }
        }
        for (name, host) in &selected {
            self.ops.require_local(name, host)?;
        }
        let lock_name = format!(
            "bump-{:x}",
            Sha256::digest(flake.as_os_str().as_encoded_bytes())
        );
        let _lock = self.ops.state.lock(&lock_name)?;
        let _system = self.ops.state.lock("system")?;
        let current = self.revision(flake)?;
        let parents = self.jj(
            &[
                "log",
                "--no-graph",
                "-r",
                "@",
                "-T",
                r#"parents.map(|p| p.commit_id()).join("\n")"#,
            ],
            flake,
        )?;
        let parents: Vec<_> = parents.lines().collect();
        require(
            parents.len() == 1,
            "bump requires a single working-copy parent",
        )?;
        for (rev, message) in [
            ("@ & conflicts()", "resolve working-copy conflicts first"),
            (
                "children(@)",
                "bump requires a working-copy change without descendants",
            ),
        ] {
            require(
                self.jj(&["log", "--no-graph", "-r", rev, "-T", "commit_id"], flake)?
                    .is_empty(),
                message,
            )?;
        }
        let edits = self.jj(&["diff", "--name-only", "-r", "@"], flake)?;
        require(
            !edits.lines().any(|l| l == "flake.lock"),
            "flake.lock has uncommitted edits",
        )?;
        require(
            !fs::symlink_metadata(flake.join("flake.lock"))?
                .file_type()
                .is_symlink(),
            "lockfile must not be a symlink",
        )?;
        let original = fs::read(flake.join("flake.lock"))?;
        if !options.no_commit {
            for policy in [
                home()?.join(".config/git/commit-style.md"),
                flake.join(".commit-style.md"),
            ] {
                if policy.is_file() {
                    progress(format!(
                        "commit policy ({}):\n{}",
                        policy.display(),
                        fs::read_to_string(&policy)?
                    ));
                }
            }
        }
        progress(format!(
            "bumping {} from parent {}; excluding unrelated working-copy edits",
            flake.display(),
            parents[0]
        ));
        let name = id("rig-bump");
        fs::create_dir_all(&self.workspace_root)?;
        let workspace = self.workspace_root.join(&name);
        require(!workspace.exists(), "workspace destination already exists")?;
        let mut validated = false;
        let mut completed = false;
        let mut created = false;
        let result = (|| -> Result<Value> {
            self.jj(
                &[
                    "workspace",
                    "add",
                    "--colocate",
                    "--name",
                    &name,
                    "-r",
                    parents[0],
                    &workspace.to_string_lossy(),
                ],
                flake,
            )?;
            created = true;
            require(
                workspace.join("flake.nix").is_file() && workspace.join("flake.lock").is_file(),
                "parent must contain flake.nix and flake.lock; commit them first",
            )?;
            let before = read_lock(&workspace)?;
            let mut update = vec!["nix".into(), "flake".into(), "update".into(), "--".into()];
            update.extend(options.inputs.clone());
            self.ops.runner.run(
                Invocation::new(update)
                    .cwd(&workspace)
                    .stream(Stream::Progress)
                    .unlimited(),
            )?;
            let after = read_lock(&workspace)?;
            let changes = self.jj(&["diff", "--name-only", "-r", "@"], &workspace)?;
            require(
                changes.lines().all(|l| l == "flake.lock"),
                "update changed files other than flake.lock",
            )?;
            if changes.is_empty() {
                completed = true;
                return Ok(json!({"changed":false,"committed":false,"flake":flake}));
            }
            let inventory = Inventory::load(self.ops.runner, &workspace)?;
            for (name, host) in &selected {
                require(
                    inventory
                        .hosts
                        .get(name)
                        .is_some_and(|h| h.managed && h.platform == host.platform),
                    "candidate changes management ownership; review before bumping",
                )?;
            }
            let managed: Vec<_> = inventory.hosts.iter().filter(|(_, h)| h.managed).collect();
            require(
                !managed.is_empty(),
                "declare managed hosts before validating an update",
            )?;
            for (name, host) in &managed {
                self.ops.runner.run(
                    Invocation::new([
                        "nix",
                        "eval",
                        "--raw",
                        NIX_FLAGS[0],
                        NIX_FLAGS[1],
                        &format!(
                            "{}#{}.drvPath",
                            workspace.display(),
                            configuration(name, host)
                        ),
                    ])
                    .unlimited(),
                )?;
            }
            let source = snapshot(self.ops.runner, &workspace, 120)?;
            eval_outputs(self.ops.runner, &source, CHECKS_EXPR, true)?;
            let candidate_ops = Operations {
                flake: workspace.clone(),
                inventory: inventory.clone(),
                runner: self.ops.runner,
                state: self.ops.state.clone(),
                names: self.ops.names.clone(),
                current_system: self.ops.current_system.clone(),
            };
            for (name, _) in &selected {
                candidate_ops.build(name, &inventory.hosts[name])?;
            }
            validated = true;
            let names = changed_inputs(&before, &after)?;
            let mut result = json!({"changed":true,"inputs":names,"flake":flake,"base":parents[0],
                "evaluatedHosts":managed.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"builtHosts":selected.iter().map(|(n,_)|n).collect::<Vec<_>>(),"committed":false,"applied":false});
            if options.no_commit {
                // No check-then-replace of a file an editor can save concurrently.
                result["candidateWorkspace"] = json!(workspace);
                return Ok(result);
            }
            require(
                self.revision(flake)? == current && fs::read(flake.join("flake.lock"))? == original,
                "checkout changed during validation; candidate retained",
            )?;
            let mut subject = format!(
                "chore(flake): bumped {}",
                if names.is_empty() {
                    "transitive inputs".into()
                } else {
                    names.join(", ")
                }
            );
            if subject.len() > 72 {
                subject = "chore(flake): bumped flake inputs".into();
            }
            let validation = if selected.is_empty() {
                "- Evaluated the managed hosts, checks and packages.".to_string()
            } else {
                format!(
                    "- Evaluated the managed hosts, checks and packages.\n- Built {}.",
                    selected
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let message = options
                .message
                .clone()
                .unwrap_or(format!("{subject}\n\n{validation}"));
            require(!message.trim().is_empty(), "commit message cannot be empty")?;
            self.jj(&["describe", "--message", &message], &workspace)?;
            let candidate = self.revision(&workspace)?;
            require(
                self.revision(flake)? == current,
                "checkout moved before application; candidate retained",
            )?;
            self.jj(&["rebase", "-r", &current, "-d", &candidate], flake)?;
            let parent = self.jj(&["log", "--no-graph", "-r", "@-", "-T", "commit_id"], flake)?;
            require(
                parent == candidate,
                "working-copy parent changed during application; inspect retained candidate",
            )?;
            result["commit"] = json!(candidate);
            result["committed"] = json!(true);
            result["applied"] = json!(true);
            completed = true;
            Ok(result)
        })();
        if completed || !validated {
            let cleanup = (|| -> Result<()> {
                if created {
                    self.jj(&["workspace", "forget", &name], flake)?;
                }
                if workspace.exists() {
                    fs::remove_dir_all(&workspace)?;
                }
                Ok(())
            })();
            if cleanup.is_err() {
                progress(format!(
                    "workspace cleanup needs attention: {}",
                    workspace.display()
                ));
            }
        }
        result.map_err(|mut e| {
            if validated {
                e.details["candidateWorkspace"] = json!(workspace);
            }
            e
        })
    }
}
fn read_lock(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path.join("flake.lock"))?)
        .map_err(|_| Error::runtime("invalid lockfile JSON"))
}
pub fn changed_inputs(before: &Value, after: &Value) -> Result<Vec<String>> {
    let inputs = |v: &Value| -> Result<serde_json::Map<String, Value>> {
        let root = v["root"]
            .as_str()
            .ok_or_else(|| Error::runtime("invalid lockfile root"))?;
        v["nodes"][root]["inputs"]
            .as_object()
            .cloned()
            .ok_or_else(|| Error::runtime("invalid lockfile inputs"))
    };
    let old = inputs(before)?;
    let new = inputs(after)?;
    let names: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    Ok(names
        .into_iter()
        .filter(|n| {
            let resolve = |lock: &Value, reference: Option<&Value>| -> Value {
                match reference.and_then(Value::as_str) {
                    Some(key) => lock["nodes"][key].clone(),
                    None => reference.cloned().unwrap_or(Value::Null),
                }
            };
            resolve(before, old.get(n)) != resolve(after, new.get(n))
        })
        .collect())
}
