# Target command contract

This is the intended complete surface. The [usage guide](usage.md) documents
the implemented subset and its narrower guarantees. Remaining stages are in
[integration](integration.md#implementation-order).

## Context and targeting

Resolve the flake from `--flake PATH`, then `RIG_FLAKE`, then `NH_FLAKE`.
Otherwise fail with a request for `--flake`; never select a repository just
because the shell happens to be inside it. Show the resolved flake and source
identity before a mutation.

Host operations default to the current machine's inventory entry. An unknown
local machine requires `--on NAME`. `--on` selects the target, not the builder.
Initially accept one name; `--all` selects every managed host in the chosen
flake. These flags are mutually exclusive.

Support `--all` for `status`, `doctor`, `build`, `diff`, `switch`, and `gc`.
Other host operations target one machine. `host list` lists the whole inventory
without probing machines. Repository operations (`bump` and secret editing or
rekeying) reject host-selection flags.

An entry delegated to another flake is listed but excluded from `--all`.
Targeting it explicitly fails with the owning-flake requirement. In particular,
the public atlas configuration must not substitute for its private work flake.
Display exclusions before proceeding; unreachable managed hosts are failures,
not exclusions.

`--builder local|target|NAME` belongs only to `build`, `diff`, and `switch`.
Default to the target machine. Select an explicit compatible builder when the
target should not build. A Mac does not implicitly cross-compile a Linux
configuration. Builders receive only the access needed to build, never the
operator's SSH agent or secret identities by default.

## System operations

| Command | Contract |
| --- | --- |
| `status` | Report reachability, configured source, last known build, active closure, and live-checkout drift. Include observation times. |
| `doctor` | Diagnose access, native backend capabilities, credentials availability, and missing prerequisites. Do not repair automatically. |
| `build` | Freeze the source for this invocation, build the target configuration, and report its closure and validation result. Do not activate. |
| `diff` | Build a candidate and compare it with the active closure. Report activation-relevant changes where the backend exposes them. |
| `switch` | Build, show the candidate diff, then activate and check the result. Do not update inputs, move bookmarks, pull, or reboot. |
| `rollback [GENERATION]` | Reactivate a retained generation, defaulting to the previous successful one. Confirm the choice before activation. |
| `bump [INPUT…]` | Update selected inputs, or all inputs when omitted; validate; commit only the resulting lock update. |
| `gc` | Preview the host's generation-retention and store-cleanup policy. Delete only with `--apply`. |

`status` performs bounded probes, not builds. A source revision alone cannot
prove that a host is current. Report desired state as unknown until a matching
build is known. Cached observations are marked stale; a disconnected host is
unreachable. Ordinary drift is a successful observation, not an exit failure.

`switch --dry-run` resolves targets and reports the intended steps without
building a system or activating it. Evaluation can still fetch inputs or build
evaluation dependencies. Use `diff` for an actual built comparison.

Local builds and switches allow dirty source, report its content identity, and
warn about live dotfiles. Remote switches require a clean, immutable source and
the [live-checkout checks](integration.md#source-and-live-files). Freeze sources
once per invocation; never resolve a moving branch independently on each host.

For `switch --all`, preflight every host and build every candidate before
activating any. Abort on a preflight or build failure. Confirm the host list and
source identities, then activate sequentially, stopping at the first failure.
Report activated, failed, unknown, and not-attempted hosts separately. Do not
undo successful hosts automatically.

An SSH disconnect during activation leaves the outcome unknown until a fresh
probe establishes it. Retrying must inspect that state before activating again.
Rollback changes a system generation; it does not undo application data,
credential rotation, live dotfile edits, or all activation-script side effects.

## Dependency updates

`bump` operates on one flake checkout. By default it evaluates that flake's
managed configurations and declared checks, then builds the current host.
`--check-host NAME` replaces the default build selection; repeat it to validate
more hosts. Each build uses its host's declared builder, or the target itself.
If the current host is absent, require a selection or `--eval-only`. Evaluation
is not a successful build or a test run.

`--eval-only` skips host builds and labels the commit's validation accordingly.
`--no-commit` retains the validated workspace for review without changing the
operator's checkout. Do not add a combined
`--switch` flag initially: deployment is a separate command with its own target
and confirmation rules.

Reject a pre-existing lock edit or unresolved conflicts. Initially require a
jj checkout with a single working-copy parent. Use that parent as the update
base in an isolated workspace, preserving unrelated working-copy edits.
Validation covers the candidate, not those unrelated edits. Show the base
revision before updating.

On success, insert the lock-only change before the working-copy change, keeping
its unrelated edits intact. `--no-commit` instead retains the isolated candidate.
Recheck the source before applying a commit; if it moved, keep the candidate
for review instead of overwriting anything.

On validation failure, leave the original checkout unchanged. Follow the
repository's commit policy. If committing fails, retain the validated candidate
and explain how to recover. A no-op update creates no commit. Never push
automatically.

## Cleanup

The host declares profile ownership and retention. The suggested starting
policy keeps generations younger than 14 days and at least three per managed
profile, plus active, booted, next-boot, and previous successful system
generations where those concepts apply. Unrelated profiles and roots are not
Rig's to remove.

`gc --apply` rechecks those protections immediately before deletion. Report
which generations will disappear and label reclaimable-byte estimates as
estimates. Serialize cleanup against Rig-managed activation and builds. If a
backend cannot enforce the protection policy, refuse the destructive operation
and report the supported native alternative. `gc --all --apply` preflights all
selected hosts, then cleans sequentially and stops on failure. Completed
cleanup cannot be undone.

Nix cleanup never prunes model files, task outputs, application data, Homebrew
packages, or arbitrary caches. `model prune` has its own preview and `--apply`.
A removed generation may no longer be available to `rollback`.

## Hosts and secrets

| Command | Contract |
| --- | --- |
| `host list` | List names, platforms, addresses, and ownership by flake without probing hosts. |
| `host show NAME` | Show one entry, including service/task registrations and native capabilities. |
| `host enroll NAME --address ADDRESS` | Verify identity and prepare a reviewable registration for an existing host declaration. Do not install an OS or grant every secret. |
| `secret list` | List logical names and recipients, never plaintext. |
| `secret edit NAME` | Edit an existing or new encrypted secret with the admin identity. New secrets require explicit recipient rules. |
| `secret recipients NAME` | Show the declared recipient policy for that secret. |
| `secret rekey [NAME…]` | Re-encrypt selected secrets, or all when omitted, after displaying recipient changes and confirming. |

Enrollment requires independent confirmation of the SSH host fingerprint.
An unauthenticated key scan is discovery, not verification. Noninteractive
enrollment requires an already trusted key or `--fingerprint SHA256:…` supplied
from a trusted source; `--yes` alone is insufficient. Already verified hosts
are a no-op; a changed key stops enrollment. Keep login authorization, host
decryption recipients, and commit-signing keys separate.

Secret editing and rekeying happen on the admin machine. Plaintext and admin
identities never enter the Nix store, commit messages, status output, or Rig
logs. Rekeying stages encrypted outputs before replacing files; a failure
preserves the originals. It does not rotate the underlying service credential
or revoke a former recipient's access to old ciphertext. Project-level fnox
secrets stay outside the initial surface.

## Models, services, and tasks

| Command | Contract |
| --- | --- |
| `model list` | Show declared models, pinned identities, availability, and known sizes. |
| `model fetch NAME` | Fetch and verify a declared artifact on the target. Reuse a verified copy; publish new files only after verification. |
| `model verify NAME` | Check installed content against the declared identity. |
| `model prune` | Preview unused Rig-managed artifacts; `--apply` removes only unreferenced, unpinned content. |
| `service list` | Show declared services with runtime and boot-policy state. |
| `service status NAME` | Inspect one service and its readiness probe, if declared. |
| `service start NAME` | Request native startup and wait for declared readiness. Missing models produce a preparation error. |
| `service stop NAME` | Request native shutdown. Explain any supervisor or socket that could restart it. |
| `service restart NAME` | Restart through the native manager and check readiness. |
| `service logs NAME` | Read target-side logs; `--follow` streams them. |
| `task list` | Show registered tasks, eligible hosts, prerequisites, and schedules. |
| `task run NAME` | Run the declared invocation in the foreground. `--detach` submits a native background job and returns its run ID. |
| `task runs NAME` | List target-side runs, outcomes, and artifact locations. |
| `task logs RUN` | Read a run's output; `--follow` streams it. |
| `task cancel RUN` | Request cancellation; report whether execution actually stopped. |

Names resolve through declarations, never by accepting arbitrary shell strings.
Host paths refer to the target. Rig does not implicitly copy mail or other
input data from the operator's machine.

Model fetches use pinned content identities, not mutable tags alone. Large
transfers report size and available disk space. An artifact without a declared
retrieval source fails with preparation instructions. Activation never downloads
weights. Pruning considers declarations, retained task runs, active consumers,
and explicit pins; files outside the managed catalog are untouched.

Service start/stop changes runtime state. Boot policy and schedules stay in Nix.
Use the declared system or user service domain; do not guess from the process
name. Model fetch, service start, and task run remain separate operations.

Tasks declare executable arguments, user, working directory, host-local inputs,
required models/services, secret references, timeout, concurrency policy, and
output retention. Default to one run per task per host. Missing prerequisites
fail before execution. Rig adds no implicit retries for non-idempotent work.
Cancellation cannot undo completed side effects.

Keep task logs and artifacts private on the target. Execution metadata stores
secret references only; task authors must prevent credential or mail contents
from leaking into logs. Schedules use systemd timers or launchd jobs, without a
Rig scheduler or queue. Generic workflow graphs and ad hoc argument passthrough
are outside the first task implementation.

## Output and confirmation

Human output identifies the host and operation. `--json` emits a versioned
result envelope, with per-host results for fleet operations; progress goes to
stderr. `--json` never implies approval. Logs are explicit opt-in output, not
part of a status response. Log commands produce text streams and initially
reject `--json`.

Exit `0` means the requested operation or observation completed; `1` means a
runtime failure or unknown outcome; `2` means invalid arguments, unsupported
capabilities, or a refused safety precondition. Fleet commands return nonzero
when any selected host fails, even if others succeeded.

`--yes` answers confirmation prompts for rollback, rekeying, enrollment, and
fleet switching. It does not waive identity checks or source/policy failures.
Cleanup requires `--apply` even with `--yes`. Without a terminal, a command that
needs confirmation fails unless explicitly approved. Run Rig as the normal
user; elevate only the native operation that requires it. Never create
passwordless access as a side effect of a command.
