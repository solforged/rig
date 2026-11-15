# Workflow walkthroughs

These examples exercise the [target command contract](commands.md), including
operations that are not implemented. Use the [usage guide](usage.md) for the
supported subset; enrollment, remote deployment, rollback, and detached tasks
remain design walkthroughs.

## Add a machine

Assume a new personal Mac called `selene`. Create its host declaration in the
system flake with the real account name and intended features. Use the existing
installation guide to prepare macOS, Nix, the account, and an SSH host key.
Enable SSH and authorize the operator's login explicitly if remote management
is wanted. A full secret-bearing activation need not have succeeded yet.

On a trusted admin machine:

```sh
rig host enroll selene --address selene.example
rig host show selene
```

Compare the discovered fingerprint with the fingerprint read at selene's
console. Enrollment prepares the address and verified public host key in the
flake for review. It does not grant secrets, add a GitHub key, or configure
commit signing. The address above is a placeholder.

Edit `secrets/agenix-rules.nix` to grant only the secrets selene needs. With an
admin YubiKey connected:

```sh
rig secret recipients berkeley-mono-variable.otf
rig secret rekey
```

Review the host, recipient, and encrypted-file changes together and commit them
with jj. Put that same revision in selene's checkout using the existing VCS
workflow. This is an explicit step because its dotfiles link into that checkout.

```sh
rig doctor --on selene
rig switch --on selene
rig status --on selene
```

A mismatched host key stops before any secret or configuration transfer. A
missing recipient produces a diagnostic instead of repeated activation
attempts. If a remote prerequisite is unavailable, finish the first activation
locally and enroll remote management afterward. OS installation and recovery
remain in the existing platform guides.

For a work machine, run the workflow against its owning private flake. A public
base configuration is not a deployable substitute for that private composition.

## Update and deploy

On hyperion, update the agents input without touching unrelated working-copy
changes:

```sh
rig bump llm-agents
```

Rig evaluates the managed configurations and declared checks, builds hyperion,
and commits the lock update. The result names the updated inputs, source
identity, validation performed, and commit. A failed validation changes neither
the main checkout's lock nor its active system.

For a broader update, build another host before accepting the lock change:

```sh
rig bump --check-host hyperion --check-host amarna
```

This updates all inputs and validates those two builds. It does not claim that
aster was built. Compatible builders and private-input access must already be
configured.

Activate the local result:

```sh
rig diff
rig switch
rig status
```

Review any unrelated local edits before switching: they are included in this
build, although `bump` did not validate them. `switch` freezes its own source
and shows its own diff. If the checkout changed since `diff`, it must not claim
to be activating the earlier candidate.

To deploy remotely, first review and synchronize the intended clean revision
and live configuration files on each target. Rig never runs an implicit pull.
Then:

```sh
rig switch --on amarna
rig status --all
```

For a coordinated personal-fleet deployment, `rig switch --all` lists the
managed hosts and excludes the delegated work configuration. It builds all
candidates before activation. An unreachable host blocks that deployment;
choose a single reachable host explicitly if that is the intended scope.

If an activation fails, the summary distinguishes hosts already activated from
those not attempted. Inspect the failing host before retrying. An SSH timeout
means the outcome is unknown, not that the old generation is still running.

```sh
rig status --on amarna
rig rollback --on amarna
```

Rollback offers the previous successful retained generation and states its
limits. It does not restore live checkout contents. Keep cleanup separate from
the update so that recovery generations remain available:

```sh
rig gc --on amarna
rig gc --on amarna --apply
```

## Operate email triage

Assume a future `mail-triage` task on aster. Its declaration invokes a separate
triage executable, reads a local inbox, and writes filing proposals into a
private run directory. The names `arete` and `mail-inference` below are proposed
registrations, not claims about installed software.

The executable owns parsing messages, prompts, output format, and approval for
any mailbox changes. Rig owns the declared execution environment and run record.
A first task should produce proposals without modifying the inbox.

Declare the model's immutable identity and retrieval source, an inference
service, and the task's dependencies in Nix. Switch aster to install those
declarations, then prepare the runtime explicitly:

```sh
rig model fetch arete --on aster
rig model verify arete --on aster
rig service start mail-inference --on aster
rig task run mail-triage --on aster
```

`model fetch` reports download size and verifies the result. The service start
waits for readiness, not just a running process. The task uses aster's inbox;
it does not upload the operator's mail over SSH.

For a run that should survive the terminal closing:

```sh
rig task run mail-triage --on aster --detach
rig task runs mail-triage --on aster
rig task logs RUN_ID --on aster --follow
rig task cancel RUN_ID --on aster
```

Replace `RUN_ID` with the returned identifier. Native service management owns
the detached process. A second overlapping run is refused by default. A failed
or interrupted run is recorded without automatically trying the email operation
again. Cancellation does not remove completed output or reverse side effects.

Add a schedule in Nix once the manual run is useful, then switch aster. Scheduled
and manual runs share the same concurrency policy and history. If the task stays
an occasional local command with no need for history, omit the registration and
invoke the workload executable directly.

Stop the inference service when it is no longer needed:

```sh
rig service stop mail-inference --on aster
```

Stopping it does not change the task's schedule. A scheduled run with a stopped
required service records a prerequisite failure; it does not silently restart
the service or download a model.
