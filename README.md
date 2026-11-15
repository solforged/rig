# Rig

Rig is a Rust CLI for a personal fleet of NixOS and nix-darwin machines. The
initial implementation supports local builds and activation, isolated jj dependency
updates, fleet inspection, pinned model downloads, native service controls,
and foreground tasks with private run history.

Remote mutations, generation pruning, rollback, enrollment, secret editing,
and detached tasks are not enabled. The CLI reports these limits before making
changes. Native activation adapters have fake-backend tests, not live-machine
switch tests.

## Run

```sh
nix run github:solforged/rig -- --help

# From a checkout:
nix run . -- --help
nix build
./result/bin/rig --version

# Build and run from source with the pinned Rust toolchain:
nix develop --command cargo run -- --help
```

The Nix outputs cover Apple silicon macOS and x86_64/aarch64 Linux. The dev
shell supplies the pinned Rust toolchain without changing your rustup defaults.

After installing the package, select a managing flake with `--flake PATH`,
`RIG_FLAKE`, or the existing `NH_FLAKE`. Declare its managed hosts before using
mutation commands; [the usage guide](docs/usage.md) covers the inventory.

```sh
rig host list
rig status --all
rig build
rig diff
rig switch --dry-run
rig bump llm-agents --no-commit
rig gc                             # preview only; retain every generation
```

Rig uses the native tools already on `PATH`, including your installed Nix,
`nh`, jj, and SSH. It does not install another Nix daemon or change your system
flake automatically. Task executables own domain behavior such as email triage;
Rig handles prerequisites, invocation, and run records.

## Develop

```sh
nix develop
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all -- --check
nix fmt -- --ci
nix build                          # package build includes the Rust tests

# Optional: real Nix protocol checks; build temporary text-file fixtures only.
cargo test --test nix_smoke --locked -- --ignored
```

Tests use fake Nix/service backends, local fixture files, temporary jj
repositories, and a loopback HTTP server for transfer-deadline checks. They
never switch systems, collect garbage, or rekey secrets.
The jj integration tests need both jj and Git; the Nix package supplies those
inside its check environment.
CI builds and tests the package natively on all three supported platforms and
runs the real-Nix fixture checks without activation.

## Guides

- [Usage](docs/usage.md): supported commands, declarations, and recovery limits.
- [Target command contract](docs/commands.md): the intended complete surface.
- [Workflow design](docs/workflows.md): enrollment, deployment, and email triage.
- [Integration](docs/integration.md): native backends and remaining work.
