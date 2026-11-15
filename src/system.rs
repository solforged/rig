//! Exact-closure local operations; remote mutation remains disabled.
use crate::{core::*, inventory::*};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::PathBuf};

pub struct Operations<'a> {
    pub flake: PathBuf,
    pub inventory: Inventory,
    pub runner: &'a dyn Runner,
    pub state: State,
    pub names: BTreeSet<String>,
    pub current_system: PathBuf,
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::runtime(format!("backend omitted {name}")))
}
impl<'a> Operations<'a> {
    pub fn new(
        flake: PathBuf,
        inventory: Inventory,
        runner: &'a dyn Runner,
        state: State,
    ) -> Result<Self> {
        Ok(Self {
            flake,
            inventory,
            runner,
            state,
            names: local_names()?,
            current_system: "/run/current-system".into(),
        })
    }
    pub fn is_local(&self, name: &str) -> bool {
        self.names.contains(name)
    }
    pub fn require_local(&self, name: &str, host: &Host) -> Result<()> {
        require(
            self.is_local(name),
            format!("remote mutation is not implemented for {name}; run Rig on that host"),
        )?;
        require(
            host.platform == local_platform(),
            format!("{name}'s platform does not match this machine"),
        )?;
        require(host.managed, format!("{name} is not managed by this flake"))
    }
    pub fn active(&self, name: &str, host: &Host) -> Result<Option<String>> {
        if self.is_local(name) {
            let active = self.runner.active_system(&self.current_system)?;
            if let Some(path) = &active {
                store_path(path)?;
            }
            return Ok(active);
        }
        let ssh = host
            .ssh
            .as_ref()
            .ok_or_else(|| Error::refusal("host has no SSH address"))?;
        let target = ssh
            .user
            .as_ref()
            .map(|user| format!("{user}@{}", ssh.host))
            .unwrap_or_else(|| ssh.host.clone());
        let output = self.runner.run(
            Invocation::new([
                "ssh",
                "-T",
                "-a",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                &ssh.port.to_string(),
                "--",
                &target,
                "nix-store --query --resolve /run/current-system",
            ])
            .timeout(20),
        )?;
        store_path(output.stdout.trim())?;
        Ok(Some(output.stdout.trim().into()))
    }
    pub fn source(&self, timeout: u64) -> Result<Value> {
        snapshot(self.runner, &self.flake, timeout)
    }
    pub fn status(&self, name: &str, host: &Host) -> Result<Value> {
        let mut result = json!({"host": name, "platform": host.platform, "observedAt": now(),
            "configuredFlake": self.flake, "managed": host.managed, "desired": "unknown",
            "liveCheckout": "unknown", "provenance": "unknown"});
        match self.active(name, host) {
            Ok(active) => {
                result["active"] = json!(active);
                result["reachability"] = json!(if self.is_local(name) {
                    "local"
                } else {
                    "reachable"
                });
            }
            Err(error) => {
                result["reachability"] = json!("unreachable");
                result["error"] = json!(error.message);
                result["code"] = json!(error.code);
                return Ok(result);
            }
        }
        if self.is_local(name) {
            if let Some(record) = self.state.read(&format!("hosts/{name}.json"))? {
                if record["state"] == "activated"
                    && !record["active"].is_null()
                    && record["active"] == result["active"]
                {
                    result["provenance"] = json!("recorded");
                }
                result["lastActivationAttempt"] = record;
            }
            let built = self.state.read(&format!("hosts/{name}-build.json"))?;
            if let Some(record) = &built {
                result["lastBuild"] = record.clone();
            }
            match self.source(20) {
                Ok(source) => {
                    if let Some(built) = built
                        .as_ref()
                        .filter(|b| b["source"]["narHash"] == source["narHash"])
                    {
                        result["desired"] = json!(if built["closure"] == result["active"] {
                            "active"
                        } else {
                            "built-not-active"
                        });
                    }
                    result["source"] = source;
                }
                Err(error) => {
                    result["sourceError"] = json!(error.message);
                    result["code"] = json!(error.code);
                }
            }
            if let Some(live) = &host.live_checkout {
                result["liveCheckout"] = json!(if live.is_dir() {
                    "present-unverified"
                } else {
                    "missing"
                });
                if let Some(built) = built.as_ref().filter(|_| {
                    live.canonicalize().ok().as_ref() == Some(&self.flake)
                        && result.get("source").is_some()
                }) {
                    result["trackedLiveSource"] = json!(if built["source"]["narHash"]
                        == result["source"]["narHash"]
                    {
                        "matches-build"
                    } else {
                        "differs-from-build"
                    });
                    result["liveCheckoutNote"] =
                        json!("ignored files and external mutable paths remain unverified");
                }
            }
        }
        Ok(result)
    }
    pub fn doctor(&self, name: &str, host: &Host) -> Result<Value> {
        let mut result = self.status(name, host)?;
        if self.is_local(name) {
            let paths: serde_json::Map<String, Value> =
                ["nix", "nix-store", "nh", "jj", "git", "ssh"]
                    .iter()
                    .map(|tool| ((*tool).into(), json!(find_executable(tool))))
                    .collect();
            let missing: Vec<_> = ["nix", "nix-store"]
                .into_iter()
                .filter(|tool| paths[*tool].is_null())
                .collect();
            result["capabilities"] = json!({"localBuild": host.managed && !paths["nix"].is_null(),
                "localSwitch": host.managed && !paths["nh"].is_null(), "remoteSwitch": false, "rollback": false,
                "generationPruning": false, "storeGC": host.platform == Platform::Nixos});
            if !missing.is_empty() {
                result["code"] = json!(1);
            }
            result["missing"] = json!(missing);
            result["executables"] = json!(paths);
        }
        Ok(result)
    }
    pub fn build(&self, name: &str, host: &Host) -> Result<Value> {
        self.require_local(name, host)?;
        let source = self.source(120)?;
        let source_path = field(&source, "path")?;
        let frozen = Inventory::from_source(self.runner, &source)?;
        require(
            frozen.hosts.get(name) == Some(host),
            "inventory changed while resolving source; retry",
        )?;
        let build_id = id("build");
        let directory = self.state.dir(format!("builds/{build_id}"))?;
        progress(format!(
            "{name}: building {} from {}",
            source["narHash"],
            self.flake.display()
        ));
        progress("live dotfiles remain outside this build's closure");
        self.runner.run(Invocation::new([
            "nix-store",
            "--add-root",
            &directory.join("source").to_string_lossy(),
            "--indirect",
            "--realise",
            source_path,
        ]))?;
        let output = run_json(
            self.runner,
            Invocation::new([
                "nix",
                "build",
                "--json",
                NIX_FLAGS[0],
                NIX_FLAGS[1],
                "--out-link",
                &directory.join("result").to_string_lossy(),
                &format!("{source_path}#{}", configuration(name, host)),
            ])
            .unlimited(),
        )?;
        let rows = output
            .as_array()
            .ok_or_else(|| Error::runtime("expected a system build result array"))?;
        require(rows.len() == 1, "expected exactly one system build result")?;
        let outputs = rows[0]["outputs"]
            .as_object()
            .ok_or_else(|| Error::runtime("build result has no outputs"))?;
        require(outputs.len() == 1, "expected exactly one system output")?;
        let closure = outputs
            .values()
            .next()
            .and_then(Value::as_str)
            .ok_or_else(|| Error::runtime("invalid system output"))?;
        store_path(closure)?;
        let record = json!({"host": name, "id": build_id, "source": source, "closure": closure, "builtAt": now(), "root": directory.join("result")});
        self.state
            .write(&format!("hosts/{name}-build.json"), &record)?;
        self.state
            .write(&format!("builds/{build_id}/record.json"), &record)?;
        Ok(record)
    }
    pub fn diff(&self, name: &str, host: &Host, built: &Value) -> Result<Value> {
        let active = self.active(name, host)?;
        let closure = field(built, "closure")?;
        let diff = if let Some(active) = &active {
            self.runner
                .run(
                    Invocation::new(["nix", "store", "diff-closures", active, closure])
                        .timeout(120),
                )?
                .stdout
        } else {
            "no active system to compare".into()
        };
        Ok(json!({"active": active, "candidate": closure, "diff": diff}))
    }
    pub fn switch(&self, name: &str, host: &Host, dry: bool) -> Result<Value> {
        self.require_local(name, host)?;
        require(
            unsafe { libc::geteuid() } != 0,
            "run Rig as your normal user; nh elevates activation only",
        )?;
        let backend = if host.platform == Platform::Darwin {
            "darwin"
        } else {
            "os"
        };
        let help = self
            .runner
            .run(Invocation::new(["nh", backend, "switch", "--help"]))?
            .stdout;
        require(
            help.contains("Path or symlink to a /nix/store path") && help.contains("--diff"),
            "installed nh does not advertise exact-store-path activation",
        )?;
        if dry {
            return Ok(json!({"host": name, "dryRun": true, "flake": self.flake,
            "steps": ["freeze source", "build rooted closure", "show diff", "activate exact closure with nh", "verify active closure"],
            "warning": "live dotfiles are outside generation rollback"}));
        }
        if let Some(previous) = self.state.read(&format!("hosts/{name}.json"))? {
            require(!matches!(previous["state"].as_str(), Some("activating" | "unknown")),
                "previous activation outcome is unresolved; inspect native processes and system state, then archive its host record before retrying")?;
        }
        let built = self.build(name, host)?;
        let comparison = self.diff(name, host, &built)?;
        let closure = field(&built, "closure")?.to_string();
        progress(format!(
            "{name}: candidate {closure}\n{}",
            comparison["diff"].as_str().unwrap_or("")
        ));
        let mut record = built;
        record["previous"] = comparison["active"].clone();
        record["state"] = json!("activating");
        record["startedAt"] = json!(now());
        self.state.write(&format!("hosts/{name}.json"), &record)?;
        let activation = self.runner.run(
            Invocation::new([
                "nh", backend, "switch", &closure, "--diff", "always", "--no-nom",
            ])
            .stream(Stream::Progress)
            .unlimited(),
        );
        let active = self.active(name, host);
        if activation.is_err() || active.as_ref().ok().and_then(|v| v.as_deref()) != Some(&closure)
        {
            record["state"] = json!("unknown");
            record["finishedAt"] = json!(now());
            if let Ok(observed) = active {
                record["observedActive"] = json!(observed);
            }
            self.state.write(&format!("hosts/{name}.json"), &record)?;
            return Err(Error::runtime(
                "activation outcome requires inspection; run rig status before retrying",
            )
            .with_details(json!({"host": name, "candidate": closure, "state": "unknown"})));
        }
        record["state"] = json!("activated");
        record["active"] = json!(closure);
        record["finishedAt"] = json!(now());
        self.state.write(&format!("hosts/{name}.json"), &record)?;
        Ok(record)
    }
    pub fn gc(&self, name: &str, host: &Host, apply: bool) -> Result<Value> {
        self.require_local(name, host)?;
        require(
            !apply || host.platform == Platform::Nixos,
            "Determinate Nix owns Mac GC; destructive collection is not implemented on Darwin",
        )?;
        let output = self
            .runner
            .run(Invocation::new(["nix-store", "--gc", "--print-dead"]).timeout(120))?;
        let dead: Vec<_> = output
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(store_path)
            .collect::<Result<_>>()?;
        let mut result = json!({"host": name, "mode": "unreachable-store-paths-only", "preview": !apply,
            "candidates": dead, "generationPruning": false, "retention": "all profiles and GC roots retained"});
        if apply {
            progress(format!(
                "{name}: collecting unreachable paths; retaining every profile"
            ));
            self.runner.run(
                Invocation::new(["nix-store", "--gc"])
                    .stream(Stream::Progress)
                    .unlimited(),
            )?;
            result["collected"] = json!(true);
            result["note"] = json!("preview is not a deletion receipt");
        }
        Ok(result)
    }
}
pub fn find_executable(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|p| p.join(name))
        .find(|p| {
            p.is_file()
                && p.metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
}
