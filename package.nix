{
  lib,
  rustPlatform,
  jujutsu,
  git,
}:

rustPlatform.buildRustPackage {
  pname = "rig-fleet";
  version = "0.1.0";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  nativeCheckInputs = [
    jujutsu
    git
  ];
  doCheck = true;
  preCheck = ''
    export HOME="$TMPDIR/rig-test-home"
    mkdir -p "$HOME"
  '';

  meta = {
    description = "Operate a personal NixOS and nix-darwin fleet";
    mainProgram = "rig";
    platforms = [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ];
  };
}
