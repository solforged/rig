# Usage

## Select a flake

Rig requires a local checkout containing `flake.nix`. Resolution is `--flake`,
then `RIG_FLAKE`, then `NH_FLAKE`. It never guesses from the current directory.
`--json` returns a versioned result envelope; progress goes to stderr. Exit codes
are `0` for completion, `1` for runtime failures or unknown outcomes, and `2`
for invalid arguments or safety refusals.

```sh
rig --flake ~/src/system host list --json
rig status --on amarna
rig status --all
```

Without a `rig` flake output, Rig discovers conventional configuration names
for inspection only. It does not treat every exposed base configuration as
permission to deploy. `--all` selects explicitly managed hosts and reports
excluded entries. SSH inspection uses strict host-key checks, batch mode,
disabled agent forwarding, and bounded probes. It never enrolls an unknown key.

## Declare managed hosts

Export an attribute set as `outputs.rig` from the managing flake. The example
in [`examples/inventory.nix`](../examples/inventory.nix) is a starting point;
derive the fields from host modules when integrating it into the system repo.
No changes to that repo are required to build or test Rig itself.

```nix
rig = {
  version = 1;
  hosts.hyperion = {
    platform = "darwin";
    managed = true;
    liveCheckout = "/Users/admin/src/system";
  };
  hosts.atlas = {
    platform = "darwin";
    managed = false;
    owner = "the private work flake";
  };
};
```

Host names select the corresponding `nixosConfigurations` or
`darwinConfigurations` attribute. `platform` is `nixos` or `darwin`.
`managed` is mandatory. Delegated entries require `owner` and cannot be mutated.
The private work flake should export its own managed atlas entry.

Optional `ssh = { host = "amarna"; user = "sol"; port = 22; };` enables remote
inspection. `liveCheckout` identifies the live dotfiles tree. Inventory
validation rejects unknown fields instead of ignoring unsupported policies.
The supported resource sections are `models`, `services`, and `tasks` under a
host, plus top-level `secrets` recipient metadata.

Rig archives the chosen flake and its locked inputs to obtain an immutable
source, then evaluates the inventory with import-from-derivation disabled.
Missing input sources can be fetched even for inspection. Keep the inventory
cheap to evaluate and free of plaintext secrets. A flake that requires a build
to evaluate its inventory fails with a diagnostic.

## Build and activate locally

```sh
rig doctor
rig build
rig diff
rig switch --dry-run
rig switch
```

`build` snapshots the flake with Nix, verifies the frozen inventory, and builds
the selected configuration from that store path. Both the source and output
are GC-rooted under Rig's state directory. `diff` compares the candidate with
`/run/current-system`. `switch` passes that exact closure to `nh`, then verifies
the active path. It does not update dependencies or pull a checkout.
On Darwin, the adapter uses `nh --expr` with a configuration-shaped expression
containing only `builtins.storePath` of that closure. `--impure` permits access
to the existing store path; it does not reevaluate the checkout. NixOS passes
the closure directly. Neither adapter substitutes a mutable flake installable.

Run as your normal user. `nh` handles privilege elevation. An interrupted or
failed activation records an unknown outcome, including any subsequent
observation of the active path. Another switch is refused while that attempt
remains unresolved. Inspect `rig status`, native activation processes, and the
system itself. After confirming that activation is no longer running and
recovering as necessary, archive `$XDG_STATE_HOME/rig/hosts/HOST.json` before
retrying (the default state base is `~/.local/state`). Rollback still requires
the platform's own tools.

Headless native commands use supervised process groups. Interactive elevation
stays in the foreground terminal group so sudo can read the terminal. Rig
cannot guarantee termination of privileged descendants when an interactive
activation is interrupted; the unresolved-attempt guard prevents an automatic
second activation.

Local dirty sources are allowed. Nix determines which tracked files enter the
source; ignored files do not become deployable merely because they are in the
checkout. Live dotfiles and application data remain outside the closure.
Status compares known built source identities where possible and labels other
live files unverified. Remote observations do not invent source provenance.

`--builder local` and `--builder target` currently both require a local target.
Other builders and all remote mutations are refused. A fleet mutation with any
remote target fails preflight before building or changing the local host.
`--dry-run` reports steps without building a system or activating it; inventory
evaluation can fetch inputs.

## Update dependencies

```sh
rig bump llm-agents
rig bump --eval-only --no-commit
rig bump nixpkgs-darwin --message 'chore(flake): bumped Darwin dependencies'
```

Bumping requires a jj checkout rooted at the flake directory, a single
working-copy parent, and no existing lock edits, conflicts, or working-copy
descendants. The parent must already contain the flake and lock. Rig creates a
colocated workspace under `~/.pi/wt/`, using that parent as the base. Unrelated
working-copy edits are excluded from validation and preserved when applying
the result.

Rig evaluates managed configurations, checks, and packages, then builds the
current host unless `--eval-only` is set. `--check-host NAME` replaces the
build selection; currently it must still identify a local host. Evaluation is
reported separately from a build and does not execute checks.

The default commit contains only the lock change and is inserted before the
working-copy change. `--no-commit` retains the validated workspace and prints
its path; it never copies over the live checkout's lockfile. Review that
workspace with jj and integrate it through your usual VCS workflow. This also
preserves lockfile saves made by an editor during validation.

A no-op creates no commit. Validation failure leaves the original checkout
unchanged; a detected concurrent edit or application failure retains the
validated workspace for review. Nothing is pushed or activated.

If bump is cancelled or fails before validation completes, Rig forgets its jj
workspace and removes the directory, even though the cancellation stays in
force. The two steps are attempted independently. If either cannot finish, Rig
reports the workspace path and what remains, such as running the printed
`jj workspace forget` command in the checkout.

Rig reads and displays global and repository commit-style files before
committing. Its default message uses `chore(flake): bumped …` with a validation
bullet. It does not interpret arbitrary prose policies; use `--message` when a
repository requires another format. Existing jj signing policy still applies.

## Cancellation and timeouts

Ctrl-C, SIGTERM, and a closing terminal (SIGHUP) cancel the running command
the same way. Cancellation is final for that process: Rig starts no further
native command, model hash, or task afterward, and nothing is retried.

- Supervised native commands and tasks run in their own process groups. Rig
  sends SIGTERM, then SIGKILL after a short grace period, so descendants that
  ignore SIGTERM stop too. Interactive elevation cannot be supervised this
  way; see the warning under
  [Build and activate locally](#build-and-activate-locally).
  On Darwin, Rig reaps an exited child before rechecking a zombie-only group's
  permission error; real signaling permission failures still stop cleanup.
- Model verification, hashing, and transfers stop without publishing partial
  content.
- Rig's own cleanup is the one exception. It still runs after cancellation,
  but every cleanup command is limited to 30 seconds. Today that means
  forgetting a bump workspace.
- Status and inventory queries have short built-in limits. Builds, updates, and
  activation run until they finish or you cancel them. Service and task
  `timeout` values come from the declaration.

## Cleanup

`rig gc` previews unreachable store paths. `rig gc --apply` currently works
only on NixOS and invokes native GC without deleting profiles or generations.
Nix rechecks reachability during collection; the earlier preview is not a
promise that the same paths will be deleted. Every existing root is retained.
On Macs, Determinate Nix remains responsible for destructive collection.

Age/count retention and generation pruning are pending. Rig build roots also
remain until explicitly removed by the operator. Check the recorded closure
and recovery needs before removing a particular build's `source` and `result`
links. Model files, run logs, and application data are never included in Nix GC.

## Model artifacts

A model declaration requires a lowercase SHA-256 hex digest. Fetching also
requires its exact byte size and an HTTPS or local `file:///…` URL:

```nix
hosts.aster.models.triage = {
  sha256 = "<64 lowercase hexadecimal characters>";
  size = 123456789;
  url = "https://example.org/pinned-weights.gguf";
};
```

Replace every placeholder with verified artifact metadata before use. Rig does
not know a canonical download for the existing Qwen blob or select an email
model on your behalf.

```sh
rig model list
rig model fetch triage
rig model verify triage
```

Files are stored by digest under `$XDG_STATE_HOME/rig/models` (default
`~/.local/state/rig/models`). Downloads stage on the same filesystem, check disk
space, enforce the size bound, and verify the digest before atomic replacement.
Existing verified content is reused. HTTP, credential-bearing URLs, and HTTPS
downgrades are refused. A single 24-hour deadline covers the HTTPS response,
including a slowly trickling body. Ctrl-C stops verification, hashing, and the
transfer without publishing a partial file. No model is downloaded during
activation or task
startup.
Model pruning and arbitrary destination paths are not implemented.

## Native services

Declare a native service on its host, for example:

```nix
services.qwen-256k = {
  manager = "systemd";
  scope = "system";
  unit = "qwen-256k.service";
  boot = false;
  timeout = 60;
  # ready = [ "/absolute/path/to/readiness-probe" ];
};
```

`service list`, `status`, `start`, `stop`, and `restart` operate locally.
Systemd supports `user` and `system` scopes; only mutations of system services
use sudo. `service logs NAME [--follow]` uses the journal.

Darwin declarations use `manager = "launchd"` and a launchd label as `unit`.
User jobs resolve in the invoking user's GUI domain; system jobs use the system
domain. Jobs must already be installed and loaded. Launchd stop requires an
explicit `restartOnExit = false`; otherwise Rig refuses a stop that may be
immediately undone. Launchd log streaming is pending.

An optional `ready` argv probe runs as the operator. Without it, Rig reports
native running state and leaves application readiness unknown. `models =
[ "triage" ];` requires verified content before starting. Boot policy is
metadata, not something these runtime commands modify. Native sockets, timers,
and other supervisors can still restart a stopped service.

`timeout` is in seconds (fractions allowed, at most 3600, default 30). It
limits the native `start`, `stop`, or `restart` command, then gives readiness
polling a separate budget of the same duration. Individual status queries and
probes retain their own limits. A native command still running at its deadline
is stopped; inspect the service before retrying because it may keep starting.

## Foreground tasks

A task declaration uses an absolute executable and working directory:

```nix
tasks.mail-triage = {
  argv = [ "/absolute/path/to/triage" "/host/local/inbox" ];
  cwd = "/host/local/work";
  timeout = 3600;
  models = [ "triage" ];
  services = [ "mail-inference" ];
  secrets.mail = "/run/agenix/mail";
};
```

The executable and its inputs must already exist. Secret entries are references
to installed readable files; Rig neither decrypts them nor injects their values.
Tasks run as the invoking user; an optional `user` field must match that user.
They inherit the operator's environment and are not sandboxed.

```sh
rig task run mail-triage
rig task runs mail-triage
rig task logs RUN_ID --follow
```

A foreground run waits for completion and saves stdout/stderr privately rather
than printing email contents into command results. `RIG_RUN_ID` and
`RIG_OUTPUT_DIR` tell the executable where to write artifacts. Run records omit
argument and environment values. Logs can still contain whatever the workload
prints, so the task author must avoid credential leakage.

Only one run per task per host is accepted. Missing prerequisites stop before
launch, and failures are never retried automatically. A timeout, Ctrl-C,
SIGTERM, or terminal hangup stops the whole process group, including
descendants that ignore SIGTERM, and records `timed-out` or `cancelled`. A
cancellation that arrives before launch, including while a model is being
verified, starts nothing and leaves no run record. If the CLI disappears
unexpectedly, such as after SIGKILL, an unfinished record is reported as
unknown and blocks another run. Inspect the original process and its children
before archiving that run directory; a recorded PID can be reused.

Detached jobs, scheduling, `task cancel`, and automatic output retention are
pending. Keep the CLI attached for these runs. A native timer may invoke the
foreground command, but Rig does not create or manage that timer.

## State and remaining commands

State is private to the invoking user under `$XDG_STATE_HOME/rig`. It contains
host/build records, rooted build links, digest-addressed model files, task run
directories, and operation locks. No central daemon is required.

`secret list` and `secret recipients NAME` read top-level declarations such as
`secrets.mail.recipients = [ "age1…" ];`. Generate those declarations from agenix
rules; they are metadata, not another source of encryption policy. Continue
using the existing secret-edit/rekey helpers for mutations.

Enrollment, rollback, secret mutations, model pruning, detached tasks, and
remote mutations fail explicitly before loading a flake or changing anything
where the command is known to be unavailable. The
[target contract](commands.md) describes the intended later behavior.
