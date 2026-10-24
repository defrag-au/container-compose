# 07 — Build plan

Status: **M0–M5 complete, 2026-10-24.** The runtime probe is done; the stack
model, parser, validator, resolver, hash and `config` command are built; the
runtime adapter, the fake runtime, `plan`, `up` (foreground and `-d`), `down`,
`ps`, `logs`, `exec`, `run`, `build`, `pull`, the supervisor, healthchecks, the
health cache and isolated networks are built. 152 tests pass, plus one ignored
real-runtime smoke test. This document records what the runtime actually does,
the design changes that follow, and the order the build goes in. It supersedes
the corresponding rows of `01`–`05` where they disagree; the deltas are listed
first so they are not missed.

The probe used `container` 1.6.0 (Homebrew) on macOS 27.0.1, arm64, with the
recommended kernel installed. Every JSON fragment below is copied from a real
run, not from the documentation. The probe's containers, volume and network were
deleted afterwards; `container list --all`, `volume list` and `network list` were
empty or back to `default` when it finished.

## Design deltas

All of these are folded into the documents they came from (`01`–`06`); this
section remains as the record of what the runtime forced and why. Each one is
marked **forced** (the runtime behaves this way) or **decided** (a choice this
build settled).

### D1 — `restart` is `no`, `on-stop` or `always` (decided)

The design's `on-failure` is replaced by **`on-stop`**: restart whenever the
service is not running, capped by `max_retries`. The name is honest about what
the tool can test. `always` is the same with no cap; `no` never restarts. The
reasoning, and why `--attach` does not rescue an exit-code test, is below.

The runtime records two container states and **no exit code**. `list --all
--format json` reports `status.state` as `running` or `stopped`, plus
`startedDate` once the container has started; `inspect` adds nothing (`status` is
exactly `{networks, startedDate, state}`). A container that ran `echo` and exited
is indistinguishable from one stopped by `container stop`.

There is one other channel: `container start --attach` propagates the
container's exit code to the process that started it (measured: `exit 7` → 7,
`exit 0` → 0, a container stopped by `container stop` → 137). It does not rescue
an exit-code policy, for the reasons below, but it is the only way the code is
ever visible.

#### Why `--attach` does not rescue an exit-code policy

The exit code is only available to the process that starts the container.

- A container already running when `up` starts (adoption) cannot be attached, so
  its exit is unattributable until the supervisor restarts it. An exit-code
  policy would silently degrade to `on-stop` for adopted services.
- An attached start without a TTY emitted roughly 16 KB of
  `failed to send signal: ["signal": 28, …]` in six seconds before the stop,
  which pollutes any stream the supervisor shares with the container.
- It does not answer the intent question either: `container stop` returns 137,
  so a manual stop is indistinguishable from a crash by exit code alone.

Intent tracking covers the tool's own stops; nothing covers anyone else's. The
supervisor therefore polls and does not use `--attach`.

Whichever policy is chosen, the discrimination rule is: **the tool knows the
stops it issued, and nothing else.** The supervisor sets an intent for a service
immediately before it stops it (Ctrl-C, failure cleanup, recreate) and clears it
on the next successful start. A `running → stopped` transition with no intent is
the policy's trigger; a transition with intent is expected and neither restarts
nor counts as a failure. While supervising, the loop never stops a service, so
the intent set is empty and the rule is simply "it stopped and we didn't do it".

A stop issued from another terminal — `container stop mitos-sieve` — is
indistinguishable from a process exit: the runtime records no stop reason and
offers no channel to attach one. Under `on-stop` or `always` it is restarted,
which is correct for the policy and surprising only if the user meant "stop it".
The documented way to stop a supervised service is to stop the supervisor or run
`down`. Honouring a manual stop would need a `container-compose stop` verb that
writes intent into the project state file for the supervisor to read — a new
verb and an IPC channel, not v1.

`down` while a supervisor runs takes the same project lock and refuses (exit 3,
naming the supervisor pid) rather than stopping containers the supervisor would
immediately try to restart.

`docs/05-testing.md`'s exit-code test is gone; it tested an unobservable
property.

### D2 — One *running* container per named volume (forced)

A named volume cannot be attached to a second **running** container, even
read-only:

```
$ container start ccprobe-ro      # ccprobe-vol mounted :ro, holder running
Error: failed to bootstrap container (cause: "internalError: "failed to
bootstrap container ccprobe-ro (cause: "unknown: "Error Domain=VZErrorDomain
Code=2 "The storage device attachment is invalid." ...
```

Deleting the volume while a container (running or stopped) references it also
fails: `volume '<name>' is currently in use and cannot be accessed by another
container, or deleted`. A **stopped** holder releases the volume — the same
`start` succeeded once the other container was stopped.

Consequences:

- Validation is "a volume is mounted by **at most one service**". The `:ro`
  exception in `01-model.md` and `02-stack-file.md` is removed.
- The mitos fixture's `spool` (rw for `sieve`, `:ro` for `ledger`) is invalid as
  written. It needs one container running both processes — the fallback `06`
  anticipated — or a host bind mount for the spool. Bind-mount sharing across
  containers is not yet tested (see open questions).

### D3 — `entrypoint` is one string, not an argv (forced)

`--entrypoint` is argv[0], and the value is not split:

```
$ container start ccprobe-ep2     # --entrypoint "/bin/echo pre"
Error: ... failed to find target executable /bin/echo pre
```

`/bin/echo` as the entrypoint plus arguments works. The schema becomes
`entrypoint = "…"`, documented as a single executable; a multi-word entrypoint
goes through an explicit `command = ["/bin/sh", "-c", "…"]` instead. This is a
change to `02-stack-file.md`.

### D4 — `start` is asynchronous (forced)

`container start` returns while the container is still `stopped`:

```
$ container start ccprobe-api
ccprobe-api
$ container list --all --format json | jq -r '.[].status.state'
stopped
$ sleep 3; container list --all --format json | jq -r '.[].status.state'
running
```

`start` also accepts exactly one container — two arguments is a usage error
(exit 64). So the start phase is per-service **and** polls `list --all` until the
service is `running`, bounded (30 s is generous; the probe took ~2–3 s). A
`depends_on: service_started` edge is satisfied by `running`, not by `start`
returning.

### D5 — Probe failures have three outcomes (applied)

Measured behaviour of `container exec`:

| probe outcome | runtime | meaning for us |
|---|---|---|
| exits 0 | exit 0 | healthy |
| exits non-zero | exit is propagated (3 for `exit 3`) | failed, counts toward `retries` |
| container not running | exit 1, `Error: container <name> is not running` | `unknown`, not unhealthy |
| command not found | exit 1, `failed to find target executable <path>` | the probe can never pass; count as failed **and** print a distinct warning naming the binary |

The last two are string matches on stderr, which is the only signal the runtime
offers. Keep the matching in one adapter function.

### D6 — A port conflict is detected at `start`, not `create` (forced)

`create` accepted a second container publishing `127.0.0.1:18184` while the
first was running; `start` failed with `Address already in use) (errno: 48)`.
`plan` can only catch duplicate host bindings **inside** the stack; a conflict
with anything else is a start failure, exit 1, with the publish spec added to
the message (as `01-model.md` already says).

### D7 — `.env` is one file, substitution only, guarded (agreed)

- One env file per stack: `[env] file`, default `.env` next to `stack.toml`.
  **No per-service files**, and `env_file` is removed from the schema.
- Scoping is explicit substitution in `stack.toml`:
  `env = { API_KEY = "${SIEVE_API_KEY}" }`. No prefix grammar.
- Resolution order: process environment, then the file. Literal `env` beats a
  substituted one.
- **Git guard**: before a command passes the values to the runtime, if the file
  is inside a worktree and `git ls-files --error-unmatch` says it is tracked, or
  `git check-ignore` says it is not ignored, refuse (exit 3) naming the fix.
  `config` warns instead so it still works without git. `--no-env-check` exists
  for the deliberate case.
- Values never reach argv: the tool passes `-e KEY` (inherit) and spawns each
  `container` child with a curated environment — the parent's minus the `.env`
  keys, plus that service's. Verified on `create`: the value arrived in the
  container, and `configuration.initProcess.environment` shows it resolved at
  create time.
- `config --json` redacts values and prints the config hash instead.

## M0 — the ten questions

| # | question | measured answer |
|---|---|---|
| 1 | Where do labels appear, and what is `list --format json`? | `configuration.labels`, on the container object **in `list --all`**. The call returns the full configuration, so ownership, config and state come from one invocation — no per-container `inspect`. |
| 2 | Does `start` restart an exited container with its original argv? | Yes. After stop/start, `/proc/1/cmdline` was `/.cz-init--sleep120` with `--init`; without init it is the original command. |
| 3 | Does `logs` return a stopped container's output? | Yes. Two containers that had exited returned their output. |
| 4 | Exit codes and stderr shapes | See the table below. All domain errors are exit 1; usage errors are 64. |
| 5 | Two containers on one volume? | No while either runs, not even `:ro` (D2). A stopped holder releases it. |
| 6 | Manifest digest in `image inspect`? | Yes: `configuration.image.descriptor.digest`, `sha256:…`, also in `list`. |
| 7 | Aliases on a custom network? | Accepted and stored in `configuration.networks[].options.aliases` and echoed in `status.networks[].hostname`. Bare-hostname DNS on a custom network remains apple/container#1809 and is not relied on. |
| 8 | Does `stop` fail on an already-stopped container? | No — exit 0, prints the name. `down` is idempotent without special-casing. |
| 9 | Does `delete --force` return before the VM is gone? | It returns with the container gone from `list`, and the network reference already released (the network delete immediately after succeeded). Good enough for `down` ordering; a volume held by the container is deletable immediately after. |
| 10 | `system version` when the apiserver is down? | JSON array with only the `container` row; the `container-apiserver` row appears when it responds. Use `system status --format json` as the gate (`"status":"running"`), and `system version` only to record. |

Also measured, and load-bearing:

- `list` **without** `--all` shows only running containers. The supervisor must
  pass `--all` or it cannot see exits.
- There are exactly two states: `running` and `stopped`. No `exited`, no
  `stopping`, no exit code, no finish time.
- `start` takes one container; `stop`, `delete`, `inspect` take several.
- `--label`, `--opt`, `--volume` and `--publish` are repeatable.
- `-e KEY` (no value) inherits from the CLI process environment and is resolved
  at create time.
- `--init` makes PID 1 the init shim, so a probe that inspects PID 1 sees the
  shim rather than the service.
- An alias containing a dot works with no `[dns] domain` configured (there is no
  `~/.config/container/config.toml` on this machine). A single-label alias needs
  the domain, per the docs.
- `container inspect` returns the same object as one element of `list`.

## Captured shapes

`container list --all --format json` — one object per container, abridged:

```json
{
  "id": "ccprobe-api",
  "configuration": {
    "labels": {
      "dev.container-compose.config": "abc123",
      "dev.container-compose.project": "ccprobe",
      "dev.container-compose.schema": "1",
      "dev.container-compose.service": "api"
    },
    "image": {
      "reference": "docker.io/library/alpine:latest",
      "descriptor": { "digest": "sha256:…", "mediaType": "…", "size": 9218 }
    },
    "initProcess": {
      "executable": "sleep",
      "arguments": ["120"],
      "environment": ["RUST_LOG=info", "PATH=…"],
      "workingDirectory": "/",
      "terminal": false,
      "user": { "id": { "uid": 0, "gid": 0 } },
      "rlimits": []
    },
    "mounts": [],
    "networks": [{
      "network": "default",
      "options": { "hostname": "ccprobe-api", "aliases": ["ccprobeapi.test"], "mtu": 1280 }
    }],
    "publishedPorts": [{
      "containerPort": 8184, "count": 1,
      "hostAddress": "127.0.0.1", "hostPort": 18184, "proto": "tcp"
    }],
    "resources": { "cpuOverhead": 1, "cpus": 2, "memoryInBytes": 536870912 },
    "useInit": true, "readOnly": false, "rosetta": false,
    "ssh": false, "virtualization": false,
    "capAdd": [], "capDrop": [],
    "dns": { "nameservers": [], "options": [], "searchDomains": [] },
    "platform": { "architecture": "arm64", "os": "linux" },
    "runtimeHandler": "container-runtime-linux",
    "sysctls": {}
  },
  "status": {
    "state": "running",
    "startedDate": "2026-10-24T02:47:21Z",
    "networks": [{
      "hostname": "ccprobe-api",
      "ipv4Address": "192.168.64.3/24",
      "ipv4Gateway": "192.168.64.1",
      "ipv6Address": "…", "macAddress": "…", "mtu": 1280,
      "network": "default", "variant": "reserved"
    }]
  }
}
```

`container volume inspect <name>` — one object per volume:

```json
[{
  "id": "ccprobe-vol",
  "configuration": {
    "name": "ccprobe-vol",
    "labels": { "dev.container-compose.project": "ccprobe" },
    "options": { "journal": "ordered", "size": "1G" },
    "sizeInBytes": 1073741824,
    "driver": "local", "format": "ext4",
    "source": "…/volumes/ccprobe-vol/volume.img",
    "creationDate": "2026-10-24T02:44:47Z"
  }
}]
```

`sizeInBytes` is the capacity; `options.size` is the string that was given. Drift
compares capacity, not the string.

`container network list --format json` — one object per network, and it already
includes `status`, so `network inspect` is not needed for reconcile:

```json
[{
  "id": "default",
  "configuration": {
    "name": "default",
    "labels": { "com.apple.container.resource.role": "builtin" },
    "mode": "nat", "plugin": "container-network-vmnet", "options": {}
  },
  "status": {
    "ipv4Gateway": "192.168.64.1",
    "ipv4Subnet": "192.168.64.0/24",
    "ipv6Subnet": "fdb4:198:d8f2:4bc9::/64"
  }
}]
```

`container system status --format json`:

```json
{
  "client": { "appName": "container", "version": "1.6.0", "build": "release" },
  "host": { "architecture": "arm64", "cpus": 18, "operatingSystem": "Version 27.0.1 (Build 26A434)" },
  "paths": { "appRoot": "~/Library/Application Support/com.apple.container/", "installRoot": "…" },
  "resources": { "containersRunning": 0, "containersTotal": 0, "images": 0 },
  "server": { "appName": "container-apiserver", "version": "1.6.0" },
  "status": "running"
}
```

## Error surface

| scenario | exit | stderr |
|---|---|---|
| name in use | 1 | `Error: container already exists: <name>` |
| no such container | 1 | `Error: container not found: <name>` |
| delete a running container without `--force` | 1 | `… container <name> is running and can not be deleted` |
| stop an already-stopped container | 0 | prints the name |
| exec in a stopped container | 1 | `Error: container <name> is not running` |
| exec a missing binary | 1 | `… failed to find target executable <path>` |
| publish a bound port | 1 at `start` | `… Address already in use) (errno: 48)` |
| delete a volume in use | 1 | `volume '<name>' is currently in use and cannot be accessed by another container, or deleted` |
| attach a volume held by a running container | 1 at `start` | `… The storage device attachment is invalid.` |
| delete a network with attached containers | 1 | `cannot delete subnet <net> with referring containers: <name>` |
| wrong argument count (two starts) | 64 | usage text |

The adapter maps every exit-1 domain error to the tool's exit 1 and keeps the
stderr verbatim; exit 64 is a usage error, never a stack problem.

## The build

### Layout

A workspace, not one package. The stack model has no runtime, no CLI and no
process spawning, so it is a library; the binary is a crate; and the adapter,
planner and supervisor join `members` as they land, without anything moving.

```
Cargo.toml            [workspace] — `crates/stack` and `crates/cli` today; members grow
crates/stack/         model, parse, validate, resolve, hash, env, sizes, durations, ports
crates/cli/           the binary: clap, commands, output, exit codes
crates/runtime/       M2: the container adapter — argv, JSON, process runner
crates/plan/          M2/M3: steps, ordering, reconcile decisions
crates/supervisor/    M4: tick, health, backoff, signal loop
dev/fake-container/
fixtures/             shared stack fixtures, readable and never generated at test time
```

Golden files live with the crate that owns the action being pinned. When `plan`
lands in M2 they move to `crates/plan/tests/golden/` (or `crates/cli/tests/golden/`
if the command stays in the binary), not to a top-level `tests/`.

No runtime trait. The fake is a binary on disk, so the adapter stays concrete and
the argv is genuinely exercised.

### Dependencies

`toml`, `serde`, `serde_json`, `sha2`, `clap`, `signal-hook`, `thiserror`,
`dotenvy`; dev: `tempfile`, `assert_cmd`, `predicates`. No async. Canonical JSON
for the hash is written by hand (sorted keys, declared array order) rather than
through `serde_json::Value`.

### M1 — scaffold and the stack model

- Workspace, flake devshell, `.envrc`, `.gitignore`, fmt/clippy/test baseline.
- `stack`: model, parse with key-path errors, the validation table, size,
  duration and port parsers (bracketed IPv6 included), resolve, canonical hash.
- Schema as amended here: `entrypoint` string, no `env_file`, one service per
  volume. `restart` and `network.mode = "isolated"` parse and describe normally,
  but `up` refuses them (exit 3, naming M4/M5) until they are implemented — a
  load-time refusal would leave `config` unable to describe a valid file.
- `[env] file` + substitution + the git guard.
- `config` and `config --json` (redacted). No runtime, no git required for
  `config`.

Exit: the validation table is green, the hash test table is green, `config` runs
on the mitos fixture, and a test asserts every error names a key path.

**Done.** `crates/stack/src/{model,parse,validate,resolve,hash,env,size,duration,ports}.rs`,
`crates/stack/src/error.rs`, and `crates/cli/src/{main,cli,error}.rs`, with 30
validation cases, 7 hash cases and 5 end-to-end `config` cases.

### M2 — fake runtime and the plan

- `dev/fake-container`: allowlist, `world.json`, `calls.jsonl`, `fail.json`.
  It must model **asynchronous start**, the two-state vocabulary, and the exact
  error strings above; `start` accepts one container and exits 64 for two.
- `scripts/capture-runtime-fixtures.sh` runs the M0 sequence and writes
  `fixtures/real/`.
- Adapter: `list --all`, `volume list`, `network list`, `system status`,
  `system version`; argv builders for create/start/stop/delete/exec/logs.
- `plan`, `plan --commands`, `plan --json`; goldens.
- Refusals are computed in the plan phase, before any pull.

Exit: `plan --commands` and `calls.jsonl` agree byte for byte; `plan` issues only
read subcommands; every refusal exits 3 before a mutating call.

**Done.** `crates/runtime/src/{command,argv,observe,labels}.rs`,
`crates/plan/src/{lib,plan}.rs`, `dev/fake-container`, and `plan` / `plan
--commands` / `plan --json` in the CLI, with 9 plan rules, 5 end-to-end plan
cases, 3 argv cases and 5 captured-shape cases.
`scripts/capture-runtime-fixtures.sh` writes `fixtures/real/` from a real
runtime, creating one labelled project and deleting exactly that project in a
trap; `crates/runtime/tests/captured.rs` parses those files through the
adapter's own types, so a runtime release that stops sending a field the tool
relies on fails a test rather than a stack.

### M3 — up, down, ps

- Reconcile network, volumes and containers per the table, with the amended
  volume rule.
- Start in waves; poll each service to `running`; `service_started` waits for
  `running`; `service_healthy` is refused until M4.
- Partial failure: stop only what this invocation started; `--keep-started`.
- `down`, `down --volumes`, ownership; `ps`, `ps --json` (health `unknown`; the
  supervisor does not exist yet).
- Port conflict at start is exit 1 with the publish spec named.
- `up` passes each service's environment to the `container` child process, never
  to its argv, so a `.env` value reaches the container as `--env KEY` (D7).
- `--no-recreate` turns a config-hash mismatch into a refusal (exit 3).
- `up` without `-d` refuses (exit 3) until the supervisor lands in M4: foreground
  `up` *is* the supervisor, and detaching silently would be a different promise.

Exit: the mitos fixture (with the spool rework) starts two real containers and
`down` removes exactly them; every adopt/recreate/refuse row has a test.

**Done.** `crates/plan/src/apply.rs` executes the plan; `up`, `down`, `ps` and
`ps --json` are in the CLI, with `--no-recreate`, `--keep-started`, `--timeout`
and `--dry-run`. The fake records the value each `--env` flag resolves to, so a
test proves the `.env` value reached the child process without appearing in an
argv. `crates/cli/tests/up.rs` covers the reconcile rows, ownership, partial
failure with and without `--keep-started`, service selection with dependencies,
the env path, the port-conflict message, and `ps` with and without the stack
file.

Verified against the real runtime (1.6.0): a two-service, two-volume stack with a
`service_started` dependency started both containers, `ps` listed both as
`running`/`unknown`, `down --volumes` stopped and deleted exactly those containers
and volumes, and a second `down` was a no-op. `container list --all` and `volume
list` were empty afterwards. The real run used an alpine stack rather than
`fixtures/mitos`, whose `mitos-local:dev` image is not built on this machine; the
engine exercised is the same. A `.env` value was also confirmed end to end:
`container inspect` showed `SMOKE_SECRET=real-secret-value` in the container's
environment while the create argv carried only `--env SMOKE_SECRET`.

### M4 — supervisor, readiness, logs

- `tick` machine, `on-stop`/`always` with backoff, 60 s reset from
  `startedDate`.
- Healthchecks with the three-outcome rule, `start_period`, `--wait`, exit 4.
- The health cache from decision C: `~/Library/Caches/container-compose/<project>.json`,
  written only by the supervisor, read only by `ps`; missing file, dead pid or
  hash mismatch means `unknown`.
- Signals, `-d` gating, project lock, `logs`.

Exit: fake-clock crash loop and backoff sequence; a stop the supervisor issued
is never restarted and a stop it did not issue is; `down` refuses while the
supervisor holds the lock; Ctrl-C ordering; a killed supervisor leaves containers
running and the next `up` adopts them.

**Done.** `crates/supervisor` holds the tick machine (pure, fake clock), the
runner, the health cache and the pid lock. `up` without `-d` now reconciles,
waits for readiness and supervises until Ctrl-C; `up -d` is refused when any
service asks for a restart. `--wait`, `--no-stop-on-exit`, `logs`,
`logs --follow`, exit 4 and exit 130 are in place. A probe that reports the
container is not running counts for nothing; one that cannot find its binary is
a failure with the binary named; failures inside `start_period` leave the state
`starting`. `down` refuses while a live supervisor holds the lock and removes a
dead project's health cache. The mitos fixture is back to `restart = "always"`
(`sieve`) and `on-stop` (`ledger`).

One delta from `03-cli.md`: `logs` has no `-f` short flag. `-f` is already the
global `--file`, and clap refuses duplicate shorts, so following is `--follow`
only.

Verified: 8 fake-clock tick tests (the 1, 2, 4, 8, 16, 30, 30 sequence, `on-stop`
give-up, intent, the 60 s reset, `start_period`, probe scheduling, and a restart
that never produces a running container), 3 clock, 4 cache, 2 lock and 3
shutdown-decision tests; 10 CLI cases for readiness, gating, logs, the cache `ps`
reads, and the lock. One integration test starts a real
child `up`, sends it SIGINT and asserts the reverse stop order and the released
lock; another does the same with `--no-stop-on-exit`. Against the real runtime, a
`restart = "on-stop"`, `max_retries = 3` service that exits after two seconds was
restarted at 1 s, 2 s and 4 s, gave up, and a SIGINT stopped it, removed the lock
and left the container stopped for `down`.

### M5 — the rest

`exec`, `run`, `build`, `pull`, `isolated` networks with the DNS caveat,
drift reporting, `ps --probe`, the real-runtime smoke test.

**Done.** The last four verbs are in the CLI. `exec` and `run` inherit the
terminal, choose `-i`/`-t` from `-i`/`-t`/`-T` and whether stdin is a TTY, and
propagate the child's exit code (a failed `exec` no longer collapses to 1).
`run` creates `<project>-<service>-run`, carries the `dev.container-compose.run`
label, is removed on exit unless `--keep`, is hidden by `ps` once stopped, and is
ignored by `down` and the plan. `build` passes `--no-cache` and prints the argv
with `--dry-run`; `pull` skips `pull = "never"` and locally built services.

Isolated networks are reconciled like any other resource: created with the
project label, kept when the subnet matches, refused when a network we own has a
different subnet or a foreign network has one that does not match; `up` prints
the apple/container#1809 caveat once, and `down` deletes the project network
after the containers. Volume size/journal drift and network subnet drift are
refusals, not silent changes. `ps --probe` runs each healthcheck now and reports
`source: "probe"`.

One delta the design did not anticipate: `run` of a service that mounts a named
volume is refused (exit 3) while that service runs, because the runtime allows a
named volume one running holder (D2). `exec` is the documented way in.

The real-runtime smoke test is `crates/cli/tests/smoke.rs`, `#[ignore]`d:

```sh
cargo test -p container-compose-cli --test smoke -- --ignored --nocapture
```

It created an isolated network, a volume and two containers, waited for
readiness, ran `ps --probe`, `exec`, `run` and `logs`, then `down --volumes`, and
asserted all three resources were gone. It passed in 34.8 s, and the machine was
left with no containers, no volumes, and only the `default` network.

## Open questions

1. **The mitos spool** — one container running both processes, or a host bind
   mount? Bind-mount sharing across containers needs one probe before the
   fixture is rewritten.
2. **Single-label aliases** without a configured DNS domain: warn in `plan` (as
   `01-model.md` says) or refuse at load, since they will fail at create?
3. Fold the deltas back into `01`–`05`, or keep `07` as the superseding
   document?
