//! Opt-in protocol checks against a configured Nix store. No activation or GC.
use rig::{core::*, inventory::*, system::Operations};
use std::{collections::BTreeSet, fs};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let flake = temp.path().join("flake");
    fs::create_dir(&flake).unwrap();
    let platform = if local_platform() == Platform::Darwin {
        "darwin"
    } else {
        "nixos"
    };
    let group = if local_platform() == Platform::Darwin {
        "darwinConfigurations"
    } else {
        "nixosConfigurations"
    };
    let source = snapshot(
        &NativeRunner,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        60,
    )
    .unwrap();
    let source_path = source["path"].as_str().unwrap();
    let system = format!(
        "{}-{}",
        std::env::consts::ARCH,
        if platform == "darwin" {
            "darwin"
        } else {
            "linux"
        }
    );
    fs::write(flake.join("flake.nix"), format!(r#"{{
      inputs.pkgSource.url = "path:{source_path}";
      outputs = {{ pkgSource, ... }}: let pkgs = pkgSource.inputs.nixpkgs.legacyPackages.{system}; in {{
        rig = {{ version = 1; hosts.local = {{ platform = "{platform}"; managed = true; }}; }};
        {group}.local.config.system.build.toplevel = pkgs.runCommand "rig-native-fixture" {{}}
          "printf '%s' 'not a system; never activate' > $out";
      }};
    }}"#)).unwrap();
    NativeRunner
        .run(Invocation::new([
            "nix",
            "flake",
            "lock",
            &flake.to_string_lossy(),
        ]))
        .unwrap();
    (temp, flake)
}

#[test]
#[ignore = "requires a configured Nix store; creates only temporary fixtures"]
fn evaluates_the_flake_root_without_a_default_package() {
    let (_temp, flake) = fixture();
    let inventory = Inventory::load(&NativeRunner, &flake).unwrap();
    assert!(inventory.hosts["local"].managed);
    assert_eq!(inventory.hosts.len(), 1);
}

#[test]
#[ignore = "requires a configured Nix store; builds a text-file fixture, never a system"]
fn freezes_and_roots_a_real_nix_build() {
    let (temp, flake) = fixture();
    let inventory = Inventory::load(&NativeRunner, &flake).unwrap();
    let ops = Operations {
        flake,
        inventory,
        runner: &NativeRunner,
        state: State::new(temp.path().join("state")).unwrap(),
        names: BTreeSet::from(["local".into()]),
        current_system: temp.path().join("never-used"),
    };
    let built = ops.build("local", &ops.inventory.hosts["local"]).unwrap();
    let closure = built["closure"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(closure).unwrap(),
        "not a system; never activate"
    );
    assert_eq!(
        fs::read_link(built["root"].as_str().unwrap())
            .unwrap()
            .to_string_lossy(),
        closure
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires Nix and nh; builds a text-file fixture, never activates"]
fn darwin_handoff_preserves_the_exact_built_closure() {
    let (temp, flake) = fixture();
    let inventory = Inventory::load(&NativeRunner, &flake).unwrap();
    let ops = Operations {
        flake,
        inventory,
        runner: &NativeRunner,
        state: State::new(temp.path().join("state")).unwrap(),
        names: BTreeSet::from(["local".into()]),
        current_system: temp.path().join("never-used"),
    };
    let built = ops.build("local", &ops.inventory.hosts["local"]).unwrap();
    let closure = built["closure"].as_str().unwrap();
    let mut command = rig::system::activation_invocation(&Platform::Darwin, closure).unwrap();
    // Build and switch share nh's installable resolver; build cannot activate.
    command.argv[2] = "build".into();
    let diff = command.argv.iter().position(|arg| arg == "--diff").unwrap();
    command.argv[diff + 1] = "never".into();
    let result = temp.path().join("native-result");
    command
        .argv
        .extend(["--out-link".into(), result.to_string_lossy().into_owned()]);
    NativeRunner.run(command).unwrap();
    assert_eq!(fs::read_link(result).unwrap().to_string_lossy(), closure);
}
