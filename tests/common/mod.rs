#![allow(dead_code)]
use rig::{core::*, inventory::*, system::Operations};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const SOURCE: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source";
pub const CLOSURE: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-system";
pub const OLD: &str = "/nix/store/cccccccccccccccccccccccccccccccc-system";
pub fn inventory() -> Value {
    json!({"version":1,"hosts":{
    "local":{"platform":local_platform(),"managed":true},
    "remote":{"platform":"nixos","managed":true,"ssh":{"host":"remote.example","user":"operator"}},
    "work":{"platform":"darwin","managed":false,"owner":"private work flake"}},
    "secrets":{"mail":{"recipients":["age1public"]}}})
}
pub struct FakeRunner {
    pub data: Mutex<Value>,
    pub calls: Mutex<Vec<Invocation>>,
    pub active: Mutex<Option<String>>,
    pub fail: Mutex<Option<String>>,
    pub after_switch: Mutex<String>,
    pub switch_fails: Mutex<bool>,
    pub service_running: Mutex<bool>,
    pub probe_fails: Mutex<bool>,
    pub jj_config: Option<PathBuf>,
    pub noop: Mutex<bool>,
    pub concurrent_edit: Mutex<Option<(PathBuf, String)>>,
}
impl FakeRunner {
    pub fn new(data: Value) -> Self {
        Self {
            data: Mutex::new(data),
            calls: Mutex::new(vec![]),
            active: Mutex::new(Some(OLD.into())),
            fail: Mutex::new(None),
            after_switch: Mutex::new(CLOSURE.into()),
            switch_fails: Mutex::new(false),
            service_running: Mutex::new(true),
            probe_fails: Mutex::new(false),
            jj_config: None,
            noop: Mutex::new(false),
            concurrent_edit: Mutex::new(None),
        }
    }
    pub fn commands(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.argv.clone())
            .collect()
    }
    pub fn did(&self, text: &str) -> bool {
        self.commands().iter().any(|a| a.join(" ").contains(text))
    }
}
impl Runner for FakeRunner {
    fn active_system(&self, _: &Path) -> Result<Option<String>> {
        Ok(self.active.lock().unwrap().clone())
    }
    fn run(&self, mut command: Invocation) -> Result<Output> {
        self.calls.lock().unwrap().push(command.clone());
        if command.argv[0] == "jj" {
            if let Some(config) = &self.jj_config {
                command
                    .env
                    .insert("JJ_CONFIG".into(), config.to_string_lossy().into());
            }
            let check = command.check;
            command.check = false;
            let result = NativeRunner.run(command)?;
            if check && result.code != 0 {
                return Err(Error::runtime(format!(
                    "fixture jj failed: {}",
                    result.stderr
                )));
            }
            return Ok(result);
        }
        let text = command.argv.join(" ");
        if self
            .fail
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| text.contains(s))
        {
            return Err(Error::runtime("fake backend failed"));
        }
        let a = &command.argv;
        let mut stdout = String::new();
        let mut code = 0;
        if a[0] == "nh" {
            if a.contains(&"--help".into()) {
                stdout = "Path or symlink to a /nix/store path --diff".into();
            } else {
                *self.active.lock().unwrap() = Some(self.after_switch.lock().unwrap().clone());
                if *self.switch_fails.lock().unwrap() {
                    return Err(Error::runtime("activation transport failed"));
                }
            }
        } else if a[0] == "ssh" {
            stdout = self.active.lock().unwrap().clone().unwrap_or_default();
        } else if a.starts_with(&["nix".into(), "flake".into(), "archive".into()]) {
            stdout = json!({"path":SOURCE,"inputs":{}}).to_string();
        } else if a.starts_with(&["nix".into(), "hash".into(), "path".into()]) {
            stdout = "sha256-test\n".into();
        } else if a.starts_with(&["nix".into(), "flake".into(), "update".into()]) {
            if !*self.noop.lock().unwrap() {
                let p = command.cwd.as_ref().unwrap().join("flake.lock");
                let mut lock: Value = serde_json::from_slice(&fs::read(&p)?).unwrap();
                lock["nodes"]["nixpkgs"]["locked"] = json!({"rev":"new","narHash":"sha256-new"});
                fs::write(p, lock.to_string())?;
            }
        } else if a.starts_with(&["nix".into(), "eval".into()]) {
            if a.contains(&"--raw".into()) {
                if let Some((path, text)) = self.concurrent_edit.lock().unwrap().take() {
                    fs::write(path, text)?;
                }
                stdout = "/nix/store/fake.drv".into();
            } else if text.contains("checks = f.checks") {
                stdout = "{}".into();
            } else {
                stdout = self.data.lock().unwrap().to_string();
            }
        } else if a.starts_with(&["nix".into(), "build".into()]) {
            stdout = json!([{"outputs":{"out":CLOSURE}}]).to_string();
        } else if a.starts_with(&["nix".into(), "store".into(), "diff-closures".into()]) {
            stdout = "package 1 -> 2\n".into();
        } else if a.contains(&"--print-dead".into()) {
            stdout = format!("{CLOSURE}\n");
        } else if a.contains(&"systemctl".into()) {
            if a.contains(&"show".into()) {
                stdout = format!(
                    "LoadState=loaded\nActiveState={}\nSubState=running\n",
                    if *self.service_running.lock().unwrap() {
                        "active"
                    } else {
                        "inactive"
                    }
                );
            } else if a.contains(&"stop".into()) {
                *self.service_running.lock().unwrap() = false;
            }
        } else if a.contains(&"launchctl".into()) {
            if a.contains(&"print".into()) {
                stdout = if *self.service_running.lock().unwrap() {
                    "state = running\n"
                } else {
                    "state = waiting\n"
                }
                .into();
            }
            if a.contains(&"kill".into()) {
                *self.service_running.lock().unwrap() = false;
            }
        } else if a[0].contains("probe") && *self.probe_fails.lock().unwrap() {
            return Err(Error::runtime("probe timed out"));
        } else if a[0].contains("false") {
            code = 1;
        }
        Ok(Output {
            code,
            stdout,
            stderr: String::new(),
        })
    }
}
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub flake: PathBuf,
    pub runner: FakeRunner,
    pub inventory: Inventory,
    pub state: State,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_data(inventory())
    }
    pub fn with_data(data: Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let flake = temp.path().join("flake");
        fs::create_dir(&flake).unwrap();
        fs::write(flake.join("flake.nix"), "{}\n").unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let inv = Inventory::parse(data.clone()).unwrap();
        Self {
            temp,
            flake,
            runner: FakeRunner::new(data),
            inventory: inv,
            state,
        }
    }
    pub fn ops(&self) -> Operations<'_> {
        Operations {
            flake: self.flake.clone(),
            inventory: self.inventory.clone(),
            runner: &self.runner,
            state: self.state.clone(),
            names: BTreeSet::from(["local".into()]),
            current_system: "/unused-test-system".into(),
        }
    }
    pub fn host(&self) -> &Host {
        &self.inventory.hosts["local"]
    }
}
pub fn shell() -> PathBuf {
    rig::system::find_executable("sh").expect("sh is required for process tests")
}
pub fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!{}\n{body}\n", shell().display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
