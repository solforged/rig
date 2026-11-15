# Integration

## Existing backends

Rig is a Rust binary packaged with `buildRustPackage` and a checked-in Cargo
lockfile. The [usage guide](usage.md) describes its current adapters; the
remaining native-platform work is listed below.

| Area | Existing mechanism | Rig's responsibility |
| --- | --- | --- |
| Builds and activation | Nix builds; `nh os` and `nh darwin` activation | Build an immutable source, root the result, and activate that exact closure. |
| Lock updates | `system-bump`, Nix, jj | Preserve update/build/commit behavior with isolated changes and explicit validation scope. |
| Secrets | agenix and admin YubiKeys | Show recipients, edit ciphertext safely, and guide enrollment. |
| NixOS cleanup | Scheduled `nh clean` | Reconcile manual cleanup with declared retention and protected generations. |
| Mac cleanup | Determinate Nix | Discover supported controls; do not introduce a competing unattended collector. |
| Services | systemd and launchd | Map declared names to native units and user/system domains. |
| Model weights | Files outside the Nix store; optional Ollama storage | Pin content and manage explicit preparation without fetching during activation. |

The current `system-bump` builds the current host by default; its evaluation
mode evaluates every host through the flake's checks. The proposed Rig default
adds managed-fleet evaluation before the selected build. The existing helper
also accepts `--switch`; Rig deliberately separates bumping from activation.

A thin wrapper cannot promise the full contract merely by forwarding arguments.
In particular, isolated lock updates and staged rekeying need more orchestration
than the current scripts provide. Keep the existing commands available during
adoption.

## Inventory

Rig reads a version-1, JSON-evaluable `rig` output in each managing flake.
Derive it from host modules instead of maintaining a separate YAML registry.
The [current schema](usage.md#declare-managed-hosts) covers ownership, platforms,
SSH addresses, live-checkout locations, and resource declarations. Configuration
attributes follow the standard host-name convention.

Per-host builders, composed-flake source mappings, and generation-retention
policies need later schema support. Unknown fields are rejected, so declaring
one of those policies cannot silently pretend to enable it.

Include public identifiers and secret references only. Keep private addresses,
workload definitions, and work-flake details in the private inventory. The
public atlas entry is delegated, and its private flake supplies the managed
entry. Rig must require that ownership declaration before enabling deployment
from an inventory that also exposes base configurations.

Without a `rig` output, discovery uses the conventional configuration names
for inspection only. Mutations require explicit ownership metadata. No inventory
has been installed into the system flake as part of this implementation.
`NH_FLAKE` preserves the existing shell convention; `RIG_FLAKE` can override it.

## Source and live files

Build records contain the source's NAR hash and store path, the resulting
closure, and its GC root. Activation records add the observed outcome. The
source includes the lock file; a jj change ID alone is not treated as immutable
source identity. Dedicated reporting of files excluded from Nix's source
filtering is still pending.

The existing Home Manager configuration uses out-of-store links into the live
checkout. A successful activation therefore does not prove that all effective
configuration matches its source revision. `status` must report system state
and live-checkout state separately.

For initial remote deployment, require the target's live configuration tree to
be clean and match the selected source. The inventory must describe how to find
that tree when an outer work flake and its system-flake input differ. Refuse
missing, changed, or unresolvable trees. Do not reset a user's checkout, replace
it with an archive, or copy ignored files as part of `switch`.

Recheck before activation and report subsequent drift. Rig cannot prevent an
external editor from changing live files during an operation. Generation
rollback cannot restore those files. Supporting immutable deployed dotfiles
would require a separate change to the system flake's linking policy.

Build and activation records are local metadata, not a central desired-state
database. Target-side records support recovery after a disconnect; missing
records mean unknown provenance, not failure of an otherwise observable native
system. Retain known-good rollback closures through explicit roots or native
profiles, not merely a path saved in a JSON file.

## Platform checks before remote deployment

Validate these against the installed tool versions before implementing their
adapters. Unsupported capabilities are errors, not approximate successes.

- Verify exact-output activation and generation rollback separately on NixOS
  and nix-darwin. Keep NixOS `boot` and `test` modes out of the common surface
  until there is a reason to expose platform-specific commands.
- Prove remote elevation and recovery without forwarding the operator's agent,
  copying credentials, or assuming passwordless sudo. Build as a normal user.
- Verify whether native cleanup can protect the declared profiles and system
  generations, including interaction with existing automatic collection.
- Establish how launchd user domains and native log retention support service
  operations and durable task runs. A loaded job is not necessarily ready.
- Test enrollment before the first secret-bearing activation. The Mac bootstrap
  currently combines prerequisite setup and activation; Rig must not invent a
  nonexistent enrollment-only bootstrap flag.

The current Qwen setup preserves a local GGUF blob rather than declaring a
canonical download. Registering it requires a verified digest and retrieval or
import policy. `model fetch` cannot invent a URL or substitute another model.
The email-triage workflow remains illustrative until its executable and artifact
are selected.

## Implementation order

| Stage | Implemented | Remaining |
| --- | --- | --- |
| 1 | Local status/build/switch/bump; store-only GC | Native switch smoke tests; generation retention; Mac destructive GC. |
| 2 | Inventory, fleet inspection, doctor, diff | Remote activation, composed live-source checks, and rollback. |
| 3 | Secret recipient metadata | Verified enrollment, editing, and staged rekeying. |
| 4 | Verified local model fetches; systemd/launchd service controls | Reference-aware model pruning, remote resources, and launchd log streaming. |
| 5 | Foreground tasks, prerequisites, private logs/history, timeout and overlap controls | Native detached submission, schedules, standalone cancellation, and output retention. |

The suite uses fake Nix/service backends, executable CLI fixtures, and real jj
workspaces in temporary repositories. It covers activation failures, concurrent
source edits, cleanup boundaries, partial fleet observations, model integrity,
and task lifecycle. The Nix package runs these tests in its build environment.
Opt-in native Nix tests check pure root-output evaluation and a rooted text-file
build through the real CLI protocol; they never activate that fixture.

Native mutation smoke tests still need separate approval on each platform.
A successful fake-backend test does not establish that an OS activation or
launchd service policy behaves as intended on a live machine.

## Current sources

These links assume the existing system checkout is beside this repository:

- [Maintenance](../../system/docs/maintenance.md) and
  [system-bump](../../system/packages/system-bump.sh).
- [Secrets](../../system/docs/secrets.md) and
  [the secret helpers](../../system/modules/common/secrets.nix).
- [Mac installation](../../system/docs/install-mac.md) and
  [atlas's work flake](../../system/docs/atlas.md).
- [Live configuration links](../../system/modules/common/default.nix).
- [Inference operations](../../system/docs/local-inference.md).
