//! Explicit, versioned ownership. Discovery alone never authorizes deployment.
use crate::core::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const NIX_FLAGS: [&str; 2] = ["--no-update-lock-file", "--no-write-lock-file"];
pub const INVENTORY_EXPR: &str = r#"f: if f ? rig then f.rig else {
 version = 1;
 hosts = (builtins.mapAttrs (name: _: { platform = "nixos"; managed = false; owner = "missing rig inventory (inspection only)"; }) (f.nixosConfigurations or {})) //
 (builtins.mapAttrs (name: _: { platform = "darwin"; managed = false; owner = "missing rig inventory (inspection only)"; }) (f.darwinConfigurations or {}));
}"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Nixos,
    Darwin,
}
pub fn local_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Darwin
    } else {
        Platform::Nixos
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub version: u32,
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub secrets: BTreeMap<String, Secret>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Host {
    pub platform: Platform,
    pub managed: bool,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub live_checkout: Option<PathBuf>,
    #[serde(default)]
    pub ssh: Option<Ssh>,
    #[serde(default)]
    pub models: BTreeMap<String, Model>,
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
}
fn default_port() -> u16 {
    22
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub recipients: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub sha256: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Manager {
    Systemd,
    Launchd,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    System,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Service {
    pub manager: Manager,
    pub scope: Scope,
    pub unit: String,
    #[serde(default)]
    pub ready: Option<Vec<String>>,
    #[serde(default = "service_timeout")]
    pub timeout: f64,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub boot: Option<bool>,
    #[serde(default)]
    pub restart_on_exit: Option<bool>,
}
fn service_timeout() -> f64 {
    30.0
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default = "task_timeout")]
    pub timeout: f64,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub user: Option<String>,
}
fn task_timeout() -> f64 {
    3600.0
}
fn absolute(path: &Path, label: &str) -> Result<()> {
    require(
        path.is_absolute() && !path.as_os_str().as_encoded_bytes().contains(&0),
        format!("{label} must be an absolute path"),
    )
}
fn argv(args: &[String], label: &str) -> Result<()> {
    require(
        !args.is_empty() && args[0].starts_with('/') && args.iter().all(|v| !v.contains('\0')),
        format!("{label} needs an absolute executable and an argv list"),
    )
}
impl Inventory {
    pub fn parse(value: serde_json::Value) -> Result<Self> {
        let result: Self = serde_json::from_value(value)
            .map_err(|e| Error::refusal(format!("invalid inventory: {e}")))?;
        result.validate()?;
        Ok(result)
    }
    pub fn load(runner: &dyn Runner, flake: &Path) -> Result<Self> {
        Self::from_source(runner, &snapshot(runner, flake, 60)?)
    }
    pub fn from_source(runner: &dyn Runner, source: &Value) -> Result<Self> {
        Self::parse(eval_outputs(runner, source, INVENTORY_EXPR, false)?)
    }
    pub fn validate(&self) -> Result<()> {
        require(self.version == 1, "inventory version must be 1")?;
        for (name, host) in &self.hosts {
            safe_name(name)?;
            require(
                host.managed || host.owner.as_ref().is_some_and(|s| !s.is_empty()),
                format!("delegated host {name} requires owner"),
            )?;
            if let Some(path) = &host.live_checkout {
                absolute(path, "liveCheckout")?;
            }
            if let Some(ssh) = &host.ssh {
                require(
                    !ssh.host.is_empty()
                        && !ssh.host.starts_with('-')
                        && ssh
                            .host
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.:[]-".contains(&c))
                        && ssh.port > 0,
                    "invalid SSH address or port",
                )?;
                if let Some(user) = &ssh.user {
                    require(
                        !user.is_empty()
                            && !user.starts_with('-')
                            && user
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
                        "invalid SSH user",
                    )?;
                }
            }
            for (name, model) in &host.models {
                safe_name(name)?;
                require(
                    model.sha256.len() == 64
                        && model
                            .sha256
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                    format!("{name} requires a lowercase SHA-256 hex digest"),
                )?;
                require(model.size != Some(0), "model size must be positive")?;
            }
            for (name, service) in &host.services {
                safe_name(name)?;
                require(
                    !service.unit.is_empty()
                        && service.unit.as_bytes()[0].is_ascii_alphanumeric()
                        && service
                            .unit
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.@:-".contains(&c)),
                    "invalid native service name",
                )?;
                require(
                    service.timeout.is_finite()
                        && service.timeout > 0.0
                        && service.timeout <= 3600.0,
                    "invalid service timeout",
                )?;
                if let Some(args) = &service.ready {
                    argv(args, "readiness probe")?;
                }
                require(
                    service.models.iter().all(|m| host.models.contains_key(m)),
                    "unknown service model dependency",
                )?;
            }
            for (name, task) in &host.tasks {
                safe_name(name)?;
                argv(&task.argv, "task")?;
                absolute(&task.cwd, "task cwd")?;
                require(
                    task.timeout.is_finite() && task.timeout > 0.0 && task.timeout <= 604800.0,
                    "invalid task timeout",
                )?;
                require(
                    task.models.iter().all(|m| host.models.contains_key(m))
                        && task.services.iter().all(|s| host.services.contains_key(s)),
                    "unknown task dependency",
                )?;
                for (key, path) in &task.secrets {
                    safe_name(key)?;
                    absolute(path, "secret reference")?;
                }
            }
        }
        for name in self.secrets.keys() {
            safe_name(name)?;
        }
        Ok(())
    }
    pub fn select(
        &self,
        name: Option<&str>,
        all: bool,
        local: &BTreeSet<String>,
        managed: bool,
    ) -> Result<Vec<(String, Host)>> {
        if all {
            let hosts: Vec<_> = self
                .hosts
                .iter()
                .filter(|(_, h)| h.managed)
                .map(|(n, h)| (n.clone(), h.clone()))
                .collect();
            require(!hosts.is_empty(), "inventory has no managed hosts")?;
            return Ok(hosts);
        }
        let name = match name {
            Some(name) => name.to_string(),
            None => {
                let matches: Vec<_> = self.hosts.keys().filter(|n| local.contains(*n)).collect();
                require(
                    matches.len() == 1,
                    "current machine is absent or ambiguous; select --on NAME",
                )?;
                matches[0].clone()
            }
        };
        let host = self
            .hosts
            .get(&name)
            .ok_or_else(|| Error::refusal(format!("unknown host: {name}")))?;
        require(
            !managed || host.managed,
            format!(
                "{name} is not managed by this flake: {}",
                host.owner.as_deref().unwrap_or("unknown owner")
            ),
        )?;
        Ok(vec![(name, host.clone())])
    }
}
/// Archive forces materialization even on Nix versions whose metadata omits
/// path/narHash. The returned source tree is immutable.
pub fn snapshot(runner: &dyn Runner, flake: &Path, timeout: u64) -> Result<Value> {
    let data = run_json(
        runner,
        Invocation::new([
            "nix",
            "flake",
            "archive",
            "--json",
            NIX_FLAGS[0],
            NIX_FLAGS[1],
            &flake.to_string_lossy(),
        ])
        .timeout(timeout),
    )?;
    let path = data["path"]
        .as_str()
        .ok_or_else(|| Error::runtime("Nix archive omitted source path"))?;
    store_path(path)?;
    let output = runner.run(
        Invocation::new(["nix", "hash", "path", "--type", "sha256", "--sri", path])
            .timeout(timeout),
    )?;
    let hash = output.stdout.trim();
    require(
        hash.starts_with("sha256-"),
        "Nix returned an invalid source hash",
    )?;
    Ok(json!({"path":path,"narHash":hash,"revision":null,"dirty":null,"flake":flake}))
}
/// An empty installable fragment selects a default package, not the flake's
/// root. Evaluate getFlake with a hash-locked reference to stay pure instead.
pub fn eval_outputs(
    runner: &dyn Runner,
    source: &Value,
    expression: &str,
    allow_ifd: bool,
) -> Result<Value> {
    let path = source["path"]
        .as_str()
        .ok_or_else(|| Error::runtime("source path is missing"))?;
    let hash = source["narHash"]
        .as_str()
        .ok_or_else(|| Error::runtime("source hash is missing"))?;
    let reference = format!(
        "path:{path}?narHash={}",
        hash.replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D")
    );
    let expression = format!(
        "({expression}) (builtins.getFlake {})",
        serde_json::to_string(&reference).unwrap()
    );
    let mut args = vec![
        "nix".into(),
        "eval".into(),
        "--json".into(),
        NIX_FLAGS[0].into(),
        NIX_FLAGS[1].into(),
    ];
    if !allow_ifd {
        args.extend(["--option", "allow-import-from-derivation", "false"].map(String::from));
    }
    args.extend(["--expr".into(), expression]);
    run_json(
        runner,
        if allow_ifd {
            Invocation::new(args).unlimited()
        } else {
            Invocation::new(args)
        },
    )
}
pub fn configuration(name: &str, host: &Host) -> String {
    format!(
        "{}.{}.config.system.build.toplevel",
        if host.platform == Platform::Darwin {
            "darwinConfigurations"
        } else {
            "nixosConfigurations"
        },
        serde_json::to_string(name).unwrap()
    )
}
pub fn flake_path(explicit: Option<&Path>) -> Result<PathBuf> {
    let path = explicit
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("RIG_FLAKE").map(PathBuf::from))
        .or_else(|| std::env::var_os("NH_FLAKE").map(PathBuf::from))
        .ok_or_else(|| Error::refusal("select --flake PATH, RIG_FLAKE, or NH_FLAKE"))?;
    require(
        path.join("flake.nix").is_file(),
        format!("no flake.nix in {}", path.display()),
    )?;
    Ok(path.canonicalize()?)
}
pub fn local_names() -> Result<BTreeSet<String>> {
    let hostname = hostname::get()?.to_string_lossy().into_owned();
    let mut names = BTreeSet::from([
        hostname.clone(),
        hostname.split('.').next().unwrap_or(&hostname).into(),
    ]);
    if cfg!(target_os = "macos") {
        if let Ok(output) = NativeRunner
            .run(Invocation::new(["/usr/sbin/scutil", "--get", "LocalHostName"]).timeout(3))
        {
            names.insert(output.stdout.trim().into());
        }
    }
    Ok(names)
}
