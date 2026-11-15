use crate::{
    bump::{BumpOptions, Bumper},
    core::*,
    inventory::*,
    resources::Resources,
    system::Operations,
};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "rig",
    version,
    about = "Operate a personal NixOS and nix-darwin fleet"
)]
pub struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    pub flake: Option<PathBuf>,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true)]
    pub yes: bool,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Args, Debug, Default)]
pub struct Target {
    #[arg(long, value_name = "HOST", conflicts_with = "all")]
    pub on: Option<String>,
    #[arg(long)]
    pub all: bool,
}
#[derive(Args, Debug, Default)]
pub struct Single {
    #[arg(long, value_name = "HOST")]
    pub on: Option<String>,
}
#[derive(Args, Debug)]
pub struct Build {
    #[command(flatten)]
    pub target: Target,
    #[arg(long, default_value = "target")]
    pub builder: String,
}
#[derive(Subcommand, Debug)]
pub enum Action {
    /// Inspect local or SSH system state.
    Status(Target),
    /// Inspect prerequisites and supported capabilities.
    Doctor(Target),
    /// Build a rooted system closure without activation.
    Build(Build),
    /// Build and compare with the active system.
    Diff(Build),
    /// Build, compare, and activate locally.
    Switch {
        #[command(flatten)]
        build: Build,
        #[arg(long)]
        dry_run: bool,
    },
    /// Preview unreachable store paths, retaining every generation.
    Gc {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        apply: bool,
    },
    /// Update inputs in an isolated jj workspace and validate.
    Bump {
        inputs: Vec<String>,
        #[arg(long = "check-host", value_name = "HOST")]
        hosts: Vec<String>,
        #[arg(long, conflicts_with = "hosts")]
        eval_only: bool,
        /// Retain the validated workspace for review; do not modify the checkout.
        #[arg(long)]
        no_commit: bool,
        #[arg(long)]
        message: Option<String>,
    },
    /// Not implemented: use native recovery tools.
    Rollback {
        generation: Option<String>,
        #[command(flatten)]
        target: Single,
    },
    #[command(subcommand)]
    Host(HostAction),
    #[command(subcommand)]
    Secret(SecretAction),
    #[command(subcommand)]
    Model(ModelAction),
    #[command(subcommand)]
    Service(ServiceAction),
    #[command(subcommand)]
    Task(TaskAction),
}
#[derive(Subcommand, Debug)]
pub enum HostAction {
    List,
    Show {
        name: String,
    },
    Enroll {
        name: String,
        #[arg(long)]
        address: String,
        #[arg(long)]
        fingerprint: Option<String>,
    },
}
#[derive(Subcommand, Debug)]
pub enum SecretAction {
    List,
    Recipients { name: String },
    Edit { name: String },
    Rekey { names: Vec<String> },
}
#[derive(Subcommand, Debug)]
pub enum ModelAction {
    List(Single),
    Fetch {
        name: String,
        #[command(flatten)]
        target: Single,
    },
    Verify {
        name: String,
        #[command(flatten)]
        target: Single,
    },
    Prune {
        #[command(flatten)]
        target: Single,
        #[arg(long)]
        apply: bool,
    },
}
#[derive(Args, Debug)]
pub struct Named {
    pub name: String,
    #[command(flatten)]
    pub target: Single,
}
#[derive(Args, Debug)]
pub struct Logs {
    pub name: String,
    #[command(flatten)]
    pub target: Single,
    #[arg(long)]
    pub follow: bool,
}
#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    List(Single),
    Status(Named),
    Start(Named),
    Stop(Named),
    Restart(Named),
    Logs(Logs),
}
#[derive(Subcommand, Debug)]
pub enum TaskAction {
    List(Single),
    Run {
        #[command(flatten)]
        task: Named,
        #[arg(long)]
        detach: bool,
    },
    Runs(Named),
    Logs(Logs),
    Cancel(Named),
}
impl Action {
    fn label(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::Doctor(_) => "doctor",
            Self::Build(_) => "build",
            Self::Diff(_) => "diff",
            Self::Switch { .. } => "switch",
            Self::Gc { .. } => "gc",
            Self::Bump { .. } => "bump",
            Self::Rollback { .. } => "rollback",
            Self::Host(_) => "host",
            Self::Secret(_) => "secret",
            Self::Model(_) => "model",
            Self::Service(_) => "service",
            Self::Task(_) => "task",
        }
    }
    fn target(&self) -> (Option<&str>, bool) {
        let single = match self {
            Self::Status(t) | Self::Doctor(t) | Self::Gc { target: t, .. } => {
                return (t.on.as_deref(), t.all)
            }
            Self::Build(b) | Self::Diff(b) | Self::Switch { build: b, .. } => {
                return (b.target.on.as_deref(), b.target.all)
            }
            Self::Model(ModelAction::List(t))
            | Self::Model(ModelAction::Fetch { target: t, .. })
            | Self::Model(ModelAction::Verify { target: t, .. })
            | Self::Model(ModelAction::Prune { target: t, .. }) => t,
            Self::Service(ServiceAction::List(t)) | Self::Task(TaskAction::List(t)) => t,
            Self::Service(
                ServiceAction::Status(n)
                | ServiceAction::Start(n)
                | ServiceAction::Stop(n)
                | ServiceAction::Restart(n),
            )
            | Self::Task(
                TaskAction::Runs(n) | TaskAction::Cancel(n) | TaskAction::Run { task: n, .. },
            ) => &n.target,
            Self::Service(ServiceAction::Logs(n)) | Self::Task(TaskAction::Logs(n)) => &n.target,
            _ => return (None, false),
        };
        (single.on.as_deref(), false)
    }
    fn preflight(&self, json: bool) -> Result<()> {
        let unsupported = matches!(
            self,
            Self::Rollback { .. }
                | Self::Host(HostAction::Enroll { .. })
                | Self::Secret(SecretAction::Edit { .. } | SecretAction::Rekey { .. })
                | Self::Model(ModelAction::Prune { .. })
                | Self::Task(TaskAction::Cancel(_))
        );
        require(
            !unsupported,
            format!(
                "{} operation is not implemented; no changes were made",
                self.label()
            ),
        )?;
        require(
            !matches!(self, Self::Task(TaskAction::Run { detach: true, .. })),
            "detached tasks are not implemented",
        )?;
        require(
            !(json
                && matches!(
                    self,
                    Self::Service(ServiceAction::Logs(_)) | Self::Task(TaskAction::Logs(_))
                )),
            "log streams do not support --json",
        )?;
        if let Self::Build(b) | Self::Diff(b) | Self::Switch { build: b, .. } = self {
            require(
                matches!(b.builder.as_str(), "local" | "target"),
                "named remote builders are not implemented",
            )?;
        }
        Ok(())
    }
}
pub fn execute(cli: &Cli, runner: &dyn Runner, state: State) -> Result<(Option<Value>, i32)> {
    cli.command.preflight(cli.json)?;
    let flake = flake_path(cli.flake.as_deref())?;
    let inventory = Inventory::load(runner, &flake)?;
    let ops = Operations::new(flake, inventory, runner, state)?;
    let resources = Resources::new(&ops);
    match &cli.command {
        Action::Host(HostAction::List) => {
            return Ok((
                Some(json!(ops
                    .inventory
                    .hosts
                    .iter()
                    .map(|(n, h)| json!({"name":n,"declaration":h}))
                    .collect::<Vec<_>>())),
                0,
            ))
        }
        Action::Host(HostAction::Show { name }) => {
            return Ok((
                Some(
                    json!({"name":name,"declaration":ops.inventory.hosts.get(name).ok_or_else(||Error::refusal("unknown host"))?}),
                ),
                0,
            ))
        }
        Action::Secret(SecretAction::List) => {
            return Ok((
                Some(json!(ops
                    .inventory
                    .secrets
                    .iter()
                    .map(|(n, s)| json!({"name":n,"recipients":s.recipients}))
                    .collect::<Vec<_>>())),
                0,
            ))
        }
        Action::Secret(SecretAction::Recipients { name }) => {
            return Ok((
                Some(
                    json!({"name":name,"recipients":ops.inventory.secrets.get(name).ok_or_else(||Error::refusal("unknown secret"))?.recipients}),
                ),
                0,
            ))
        }
        Action::Bump {
            inputs,
            hosts,
            eval_only,
            no_commit,
            message,
        } => {
            let bumper = Bumper {
                ops: &ops,
                workspace_root: home()?.join(".pi/wt"),
            };
            return Ok((
                Some(bumper.run(&BumpOptions {
                    inputs: inputs.clone(),
                    hosts: hosts.clone(),
                    eval_only: *eval_only,
                    no_commit: *no_commit,
                    message: message.clone(),
                })?),
                0,
            ));
        }
        _ => (),
    }
    let (target, all) = cli.command.target();
    let read_only = matches!(&cli.command, Action::Status(_) | Action::Doctor(_));
    let selected = ops.inventory.select(target, all, &ops.names, !read_only)?;
    if all {
        for (n, h) in &ops.inventory.hosts {
            if !h.managed {
                progress(format!(
                    "excluding {n}: {}",
                    h.owner.as_deref().unwrap_or("delegated")
                ));
            }
        }
    }
    if !read_only {
        for (n, h) in &selected {
            ops.require_local(n, h)?;
        }
        if matches!(&cli.command, Action::Gc { apply: true, .. }) {
            require(
                selected.iter().all(|(_, h)| h.platform == Platform::Nixos),
                "Determinate Nix owns Mac GC; destructive collection is not implemented on Darwin",
            )?;
        }
        if all && matches!(&cli.command, Action::Switch { dry_run: false, .. }) {
            confirm(
                &format!("Activate selected hosts from {}?", ops.flake.display()),
                cli.yes,
            )?;
        }
    }
    let mut results = Vec::new();
    let mut code = 0;
    for (index, (name, host)) in selected.iter().enumerate() {
        let result = (|| -> Result<Option<Value>> {
            Ok(Some(match &cli.command {
                Action::Status(_) => ops.status(name, host)?,
                Action::Doctor(_) => ops.doctor(name, host)?,
                Action::Build(_) => {
                    let _lock = ops.state.lock("system")?;
                    ops.build(name, host)?
                }
                Action::Diff(_) => {
                    let _lock = ops.state.lock("system")?;
                    let built = ops.build(name, host)?;
                    json!({"build":built,"comparison":ops.diff(name,host,&built)?})
                }
                Action::Switch { dry_run, .. } => {
                    let _lock = ops.state.lock("system")?;
                    ops.switch(name, host, *dry_run)?
                }
                Action::Gc { apply, .. } => {
                    let _lock = ops.state.lock("system")?;
                    ops.gc(name, host, *apply)?
                }
                Action::Model(ModelAction::List(_)) => resources.models(name, host)?,
                Action::Model(ModelAction::Fetch { name: model, .. }) => {
                    resources.fetch(name, host, model)?
                }
                Action::Model(ModelAction::Verify { name: model, .. }) => {
                    resources.verify(name, host, model)?
                }
                Action::Service(ServiceAction::List(_)) => json!(host
                    .services
                    .keys()
                    .map(|s| resources.service_status(name, host, s, true))
                    .collect::<Result<Vec<_>>>()?),
                Action::Service(ServiceAction::Status(n)) => {
                    resources.service_status(name, host, &n.name, true)?
                }
                Action::Service(
                    ServiceAction::Start(n) | ServiceAction::Stop(n) | ServiceAction::Restart(n),
                ) => {
                    let _lock = ops.state.lock(&format!("service-{name}-{}", n.name))?;
                    let action = match &cli.command {
                        Action::Service(ServiceAction::Start(_)) => "start",
                        Action::Service(ServiceAction::Stop(_)) => "stop",
                        _ => "restart",
                    };
                    resources.service(name, host, &n.name, action)?
                }
                Action::Service(ServiceAction::Logs(logs)) => {
                    resources.service_logs(name, host, &logs.name, logs.follow)?;
                    return Ok(None);
                }
                Action::Task(TaskAction::List(_)) => resources.task_list(name, host)?,
                Action::Task(TaskAction::Run { task, .. }) => {
                    resources.task_run(name, host, &task.name)?
                }
                Action::Task(TaskAction::Runs(task)) => {
                    json!(resources.task_runs(name, host, &task.name)?)
                }
                Action::Task(TaskAction::Logs(logs)) => {
                    resources.task_logs(name, host, &logs.name, logs.follow)?;
                    return Ok(None);
                }
                _ => return Err(Error::refusal("operation is not implemented")),
            }))
        })();
        match result {
            Ok(Some(value)) => {
                code = code.max(value.get("code").and_then(Value::as_i64).unwrap_or(0) as i32);
                results.push(json!({"host":name,"result":value}));
            }
            Ok(None) => return Ok((None, 0)),
            Err(error) => {
                code = code.max(error.code);
                results.push(json!({"host":name,"error":error.message,"code":error.code,"details":error.details}));
                if !read_only {
                    for (name, _) in &selected[index + 1..] {
                        results.push(json!({"host":name,"state":"not-attempted"}));
                    }
                    break;
                }
            }
        }
    }
    Ok((Some(json!(results)), code))
}
fn render(value: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                println!("{pad}(none)");
            }
            for item in items {
                render(item, indent);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                if item.is_object() || item.is_array() {
                    println!("{pad}{key}:");
                    render(item, indent + 2);
                } else {
                    println!(
                        "{pad}{key}: {}",
                        item.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| item.to_string())
                    );
                }
            }
        }
        _ => println!("{pad}{value}"),
    }
}
pub fn main_entry() -> i32 {
    let json_output = std::env::args_os().any(|v| v == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return 0;
            }
            if json_output {
                println!(
                    "{}",
                    json!({"version":1,"ok":false,"error":error.to_string(),"code":2})
                );
            } else {
                let _ = error.print();
            }
            return 2;
        }
    };
    let result = (|| {
        install_signals()?;
        execute(&cli, &NativeRunner, State::from_env()?)
    })();
    match result {
        Ok((data, code)) => {
            if let Some(data) = data {
                if cli.json {
                    println!(
                        "{}",
                        json!({"version":1,"command":cli.command.label(),"ok":code==0,"results":data})
                    );
                } else {
                    render(&data, 0);
                }
            }
            code
        }
        Err(error) => {
            if cli.json {
                println!(
                    "{}",
                    json!({"version":1,"command":cli.command.label(),"ok":false,"error":error.message,"code":error.code,"details":error.details})
                );
            } else {
                progress(&error.message);
                if error.details != json!({}) {
                    progress(error.details);
                }
            }
            error.code
        }
    }
}
