# 05 — Testing

Status: design, 2026-10-24.

The interesting behaviour of this tool is not "does a VM boot" — the runtime
owns that. It is the decisions: what to create, what to refuse, in what order,
and which argv is issued. Those are testable without a runtime, and they are
tested first.

## The shape

```
unit          parsing, validation, hashing, ordering, the supervisor state machine
contract      golden plans and recorded argv, against a fake runtime
integration   the real runtime, opt-in, `#[ignore]` by default
```

The middle layer is the one that would otherwise not exist. A fake runtime that
records argv turns "the plan says X" into "the process was invoked with exactly
X", and it costs a few hundred lines.

## The fake runtime

A small Rust binary in `dev/fake-container`, `publish = false`, built by a
normal `cargo build`. Tests put a copy of it on `PATH` under the name
`container`:

```
<tmp>/bin/container -> target/debug/fake-container
```

and point the tool at it with `--runtime` or by setting `PATH`, so `plan
--commands` still prints `container` — the argv is honest while the process is
fake.

### Behaviour

The fake reads and writes one directory, `FAKE_CONTAINER_DIR` (a temp dir per
test):

| file | contents |
|---|---|
| `world.json` | the simulated resources: networks, volumes, containers, images. |
| `calls.jsonl` | one JSON object per invocation: `program` (the resolved path), `argv` (everything after it), `cwd`, selected env, timestamp (a counter, not a clock). |
| `fail.json` | optional failure injection: `{"subcommand": "create", "match": "mitos-ledger", "exit": 1, "stderr": "..."}`. |

It implements the subset in [`04-runtime.md`](04-runtime.md#minimum-surface-the-tool-requires),
plus `list`, `logs` and `system version`. `create` records the container as
`stopped`; `start` as `running`; `stop` as `exited`; `delete` removes it. IDs are
sequential (`c0001`, `c0002`) so a golden file never depends on a UUID.

Two rules keep it honest:

- **Unknown flags and subcommands fail loudly** (exit 64). The fake validates
  each argv against an allowlist derived from the command reference. If the
  adapter invents a flag, a test fails instead of the runtime rejecting it on a
  user's machine.
- **It never pretends to do work.** No image layers, no processes, no network.
  If a test needs the fake to simulate something (a container exiting, a
  volume refusing deletion), that is added to `world.json` explicitly.

The fake is a test double, not a shim. It never ships in a release build.

### What the contract tests assert

1. **The plan is the argv.** For each fixture, `plan --commands` is compared to a
   committed golden file, and then `up` is run against the fake and `calls.jsonl`
   is compared to the same file. The two must agree; a change in either is a
   deliberate golden update. `program` is asserted separately, so a test proves
   the binary invoked was the one on `PATH` and not a leftover real `container`.
2. **Plan makes no changes.** After `plan`, `calls.jsonl` contains only read
   subcommands: `list`, `inspect`, `system version`.
3. **Reconcile decisions.** For each row of the adopt/recreate/refuse table in
   [`01-model.md`](01-model.md#adopt-recreate-or-refuse), a `world.json` is set up
   and the exact sequence of mutating calls is asserted. An adopted container
   produces no `create`; a config-hash mismatch produces `stop`, `delete`,
   `create` in that order; `--no-recreate` produces none and exits 3.
4. **Ownership.** A container with the right name and no project label is never
   deleted. A labelled orphan is. An unlabelled volume is never deleted, even
   with `--volumes`.
5. **Ordering.** A → B → C starts C, B, A and stops A, B, C. Independent services
   are ordered by name so two runs of the same stack produce the same file.
6. **Failure.** A create failure stops the services started by that invocation
   and leaves the pre-existing ones alone; `--keep-started` leaves the new ones
   running; the exit code is 1 and the failing service is named in stderr.
7. **Readiness timeout.** The fake never makes a probe succeed; `up` exits 4 and
   names the service and the elapsed budget.
8. **Exit codes.** One test per code in [`03-cli.md`](03-cli.md#exit-codes),
   asserting both the code and that nothing was mutated where the design says
   nothing was.
9. **Streams.** `--json` writes one parseable document to stdout and nothing
   else — progress lines are on stderr.

### Golden files

```
tests/golden/
    minimal.commands
    minimal.plan
    mitos.commands
    mitos.plan
```

`minimal` is an alpine echo service: the smallest stack that exercises create,
start and stop. `mitos` is the local stack from
[`06-roadmap.md`](06-roadmap.md#the-first-fixture), with two services, three volumes,
a healthcheck and a dependency.

Goldens are regenerated only with an explicit flag (`UPDATE_GOLDEN=1`), and the
diff is read before it is committed — the same posture as the generated widget
catalogue in `shared-crates`. A golden that changes because of a refactor is
fine; a golden that changes because a flag was dropped is the bug the file
exists to catch.

The comparison normalises the repository root to `<root>` and the temp directory
to `<tmp>`, because the argv must contain absolute host paths for bind mounts
and the config hash covers them. Normalising is the only transformation: flags,
order and spacing are compared as written.

## Unit tests

**Validation.** A table of invalid stacks, one per rule in
[`02-stack-file.md`](02-stack-file.md#validation). Each case asserts the exact
key path appears in the message (`[services.sieve].depends_on`) and that the
load fails, not warns. Plus one valid stack that sets every key, so a new key
cannot be added without a parser change.

**The config hash.** Same model, keys reordered and comments changed → same
hash. One env value changed → different hash. `depends_on` changed → same hash.
A relative bind path in a different working directory → same hash, because it
resolves against the stack file. A moved stack file → different hash only if a
relative path resolved differently, which is asserted explicitly.

**Ordering.** Topological sort with a deterministic tie-break; a cycle names the
services in the cycle; a self-dependency is a distinct error.

**Size and duration parsing.** `512M`, `1G`, `512MiB`, `1g` per the runtime's
documented suffixes; reject `1 GiB`. Durations accept `ms`, `s`, `m`, `h`.

**The supervisor.** A pure state machine:

```rust
enum Action { Restart { service: String }, MarkUnhealthy { .. }, HealthCheck { .. } }

fn tick(&mut self, now: Instant, observed: &[Observed]) -> Vec<Action>;
```

`now` is passed in, so a crash loop is tested by advancing a fake clock, not by
sleeping. Assertions: the backoff sequence is 1, 2, 4, 8, 16, 30, 30; `always`
never gives up; `on-failure` gives up after `max_retries`; a container that
stays up for 60 s resets the backoff; an exit code of 0 does not restart under
`on-failure`; a service in `start_period` is not reported unhealthy.

**Signal handling.** The signal loop is a function of an event channel, so tests
push `SigInt` and assert the stop order, then push a second and assert `kill`.
No test sends a real signal to the test process.

## Integration tests

Real-runtime tests are `#[ignore]` and run explicitly:

```sh
cargo test -- --ignored
CONTAINER_COMPOSE_REAL=1 cargo test --test smoke
```

They require `container` installed, `container system start`, and macOS 26. On a
machine without it, the binary is absent and the tests skip with a printed
reason rather than fail — a test that fails on a design machine teaches people
to ignore failures.

What is genuinely integration-only, and is therefore tested here rather than
against the fake:

| behaviour | why it cannot be faked |
|---|---|
| VM boot and start latency | `start` returning is not the process running. |
| bind mount visibility and uid/gid mapping | virtiofs semantics and ownership are the runtime's. |
| published ports reachable from loopback | the proxy is outside the container. |
| `.test` name resolution and aliases | the DNS service is host-level, with an `/etc/resolver` entry. |
| volume sharing between containers | undocumented; the answer decides the one-writer rule. |
| `--cpus` / `--memory` actually limiting | cgroup enforcement inside the VM. |
| `exec` with a TTY | terminal handling. |
| log following after a container stops | runtime buffering. |
| `delete` / `stop` on a container that is gone | the real error text and exit code. |

The smoke test is one stack: `alpine` with `sleep`, a bind mount, a published
port, a `tmpfs`, and a healthcheck that succeeds. It asserts the things a fake
cannot: the file is visible in the mount, the port answers, `ps` says `healthy`,
`down` removes the container, and the volume survives unless `--volumes` is
given.

Integration tests use a project name with a per-run suffix
(`cc-smoke-<pid>`), so a failed run cannot collide with the next one, and they
clean up in a `Drop` guard. They never use `--all` or `prune`; if they leak, the
leak is named in the failure output.

## Test layout

```
src/…                       unit tests in `#[cfg(test)] mod tests`
tests/plan.rs               golden plans and argv
tests/reconcile.rs          adopt / recreate / refuse
tests/supervisor.rs         tick-driven state machine
tests/validation.rs         table-driven invalid stacks
tests/smoke.rs              #[ignore], real runtime
tests/golden/               committed fixtures
dev/fake-container/         the test double
```

Fixtures live in the repository, are readable, and are never generated at test
time. No test reaches the network.

## What is not tested, and why

- **The runtime's own correctness.** A `container` bug is upstream's; the tool
  tests that it passes the right argv and reacts to the documented output.
- **Timing of a real boot in a unit test.** Only the smoke test observes it, and
  it asserts a generous bound, not an exact duration.
- **A second macOS version.** The runtime supports macOS 26 and later; the
  design targets the installed version and records it in `plan --json`.
