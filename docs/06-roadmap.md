# 06 — Roadmap

Status: design, 2026-10-24. Nothing is built. This document is the brief for the
build session, and the place to record what changes when the runtime answers a
question the design could not.

## Milestones

Each milestone is a working binary, not a layer of scaffolding. The order is set
by what the first fixture needs, not by what is easiest to write.

### M1 — plan, up, down, ps

**Scope**

- `stack.rs`: parse and validate [`02-stack-file.md`](02-stack-file.md).
- `config` and `plan`, including `--commands` and `--json`.
- `up` without supervision: pull, reconcile network and volumes, adopt or
  recreate containers, create, start in dependency waves.
- `down`, including `--volumes` and the ownership refusal.
- `ps`.
- On a failed start, the last 20 log lines of the failing service, so a failure
  is diagnosable without a second command. The `logs` command itself is M2.
- The fake runtime and the golden files from
  [`05-testing.md`](05-testing.md), written before the reconcile code.
- `restart` other than `"no"` and `network.mode = "isolated"` parse and
  describe normally, but `up` refuses them (exit 3, naming M2/M3) until they are
  implemented — a load-time refusal would leave `config` unable to describe a
  valid file. `depends_on: service_healthy` is refused the same way until M2.

**Exit criteria**

- The mitos fixture stack, with both services set to `restart = "no"`, plans
  and starts two real containers, and `down` removes exactly them.
- Every row of the adopt/recreate/refuse table has a test.
- `plan --commands` and the fake runtime's recorded argv are byte-identical.
- `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` are clean, with no
  ignored test that is not an integration test.

**Deliberately absent**: health waiting, restart, `logs`, `exec`, `run`, `build`,
`pull`, isolated networks. M1 starts containers and tells you the truth about
what it did.

### M2 — supervision and readiness

**Scope**

- The `tick` state machine from [`01-model.md`](01-model.md#the-supervisor-decision).
- Healthchecks, `start_period`, `--wait`, and exit code 4.
- `restart` policies with backoff, in the foreground supervisor.
- `up -d` gating, Ctrl-C semantics, `--no-stop-on-exit`, `--keep-started`.
- `logs` (the supervisor is already polling; log following is the next thing a
  crash loop needs).
- A project lock, so two `up`s cannot supervise the same containers. A second
  `up` on a locked project either attaches (if it is the same terminal) or exits
  3; the lock records a pid and is ignored when that pid is gone.
- The fixture flips back to `restart = "always"` / `on-stop`, and that is the
  version the local-stack session runs.

**Exit criteria**

- A service that exits immediately is restarted on the documented backoff and
  gives up after `max_retries` under `on-stop`, all in a unit test with a fake
  clock.
- A service whose healthcheck fails during `start_period` is not reported
  unhealthy, and one that never becomes healthy makes `up --wait` exit 4.
- Ctrl-C stops services in reverse order; the second Ctrl-C kills.
- Killing the supervisor leaves the containers running, and a new `up` adopts
  them without recreating.

### M3 — the rest of the surface

**Scope**

- `exec`, `run`, `build`, `pull`.
- Adoption and drift reporting for resources whose stack entry changed shape
  (a volume size, a network subnet).
- `network.mode = "isolated"`, with the DNS caveat printed, and aliases.
- `ps --json` completeness, `config --json` as the hash input.
- The integration smoke test from [`05-testing.md`](05-testing.md).

**Exit criteria**

- `container-compose up` on a clean machine, then `down --volumes`, leaves the
  runtime’s `container list --all` and `volume list` exactly as they were.
- An isolated stack starts, and its services reach each other by IP; the caveat
  says so in the plan.
- `build` and `pull` produce argv identical to `plan --commands`.

### M4 — out of scope

Written down so they are decisions rather than omissions:

- **Compose compatibility**, in either direction.
- **Remote runtimes.** SSH to a Linux box, a second Mac, anything where the
  `container` binary is not local. The argv is the same; the file paths, the
  bind mounts and DNS are not, and pretending otherwise is how a local tool
  becomes a broken remote one.
- **A launchd unit generator.** The honest detached mode. It needs the
  supervisor to be crash-safe and to write a log, and that is M2 work first.
- **Secrets.** No vault, no SOPS, no file provider. The one `[env]` file,
  gitignored and checked by git, is the whole story.
- **Multi-user.** A shared Mac with two people's stacks is a naming and
  permission problem, not an orchestration one.
- **Kubernetes anything.** `container cluster` exists; it is not this tool.

## The first fixture

`mitos/deploy/local/`, built as part of the local-stack session, not this one.

The aim is hermetic: one host snapshot, two processes, no network dependency
after the snapshot is on disk. `wallet-sieve` follows the tip and writes the tail
spool; `token-ledger` reads the snapshot and the spool.

### Host: fetch and build

```sh
# once, for the shared spool
mkdir -p mitos/deploy/local/spool

# once per snapshot
mkdir -p mitos/deploy/local/snapshot
export AGGREGATOR_ENDPOINT=https://aggregator.release-mainnet.api.mithril.network/aggregator
export GENESIS_VERIFICATION_KEY=<the mainnet key in infra/services/market-ledger/snapshot-refresh.sh>
mithril-client cardano-db download latest --download-dir mitos/deploy/local/snapshot

# once per code change, from the mitos root
nix develop -c cargo zigbuild --release --target aarch64-unknown-linux-musl \
    -p wallet-sieve -p token-ledger
mkdir -p mitos/deploy/local/bin
cp target/aarch64-unknown-linux-musl/release/{wallet-sieve,token-ledger} mitos/deploy/local/bin/

# once per image change
container build -t mitos-local:dev mitos/deploy/local
```

The image is built on the host and referenced by both services with
`pull = "never"`. One image, both binaries: if concurrent volume attachment
turns out not to work, the fallback is one container running both processes, and
that is a two-line change to the stack file rather than a new image.

`mithril-client` refuses to unpack into a non-empty `<download-dir>/db`, and the
snapshot lands at `snapshot/db/immutable/`. The bind mount below points at
`snapshot`, so the container sees `/chain/db/immutable`.

### `mitos/deploy/local/Containerfile`

```dockerfile
FROM alpine:3.22
RUN apk add --no-cache ca-certificates curl
COPY bin/wallet-sieve /usr/local/bin/wallet-sieve
COPY bin/token-ledger /usr/local/bin/token-ledger
WORKDIR /data
```

`curl` is for the healthchecks; the tool runs an argv, so the probe has to be a
binary. Alpine is multi-arch, and the two copied binaries are aarch64-musl, which
is what the VM runs on Apple silicon.

### `mitos/deploy/local/.dockerignore`

```
snapshot/
spool/
```

The build context is `deploy/local`, and the snapshot lives inside it. Without
this file every `container build` sends the whole chain database to the builder
— tens of gigabytes for a file the image never uses. (BuildKit reads
`.dockerignore`; confirm the builder honours it on first use.)

### `mitos/deploy/local/stack.toml`

```toml
version = 1
name = "mitos"

[volumes.cache]
size = "20G"

[volumes.ledger]
size = "300G"

[services.sieve]
image = "mitos-local:dev"
pull = "never"
command = [
  "wallet-sieve", "serve",
  "--listen", "0.0.0.0:8184",
  "--db", "/cache/sieve.db",
  "--immutable", "/chain/db/immutable",
  "--tail-db", "/spool/tail.db",
]
env = { RUST_LOG = "info,wallet_sieve=debug" }
volumes = ["./snapshot:/chain:ro", "./spool:/spool", "cache:/cache"]
ports = ["127.0.0.1:8184:8184"]
cpus = 4
memory = "8G"
ulimits = ["nofile=65536:65536"]
restart = "always"

[services.sieve.healthcheck]
exec = ["curl", "-fsS", "http://127.0.0.1:8184/health"]
interval = "15s"
timeout = "5s"
start_period = "20s"
retries = 4

[services.ledger]
image = "mitos-local:dev"
pull = "never"
command = [
  "token-ledger", "serve",
  "--listen", "0.0.0.0:8185",
  "--data-dir", "/chain/db",
  "--tokens", "/config/tokens.toml",
  "--db-dir", "/db/auto",
  "--archive-dir", "/db/archive",
  "--tail-db", "/spool/tail.db",
  "--tip-refresh-secs", "60",
  "--walk-workers", "2",
  "--seek-workers", "4",
]
env = { RUST_LOG = "info" }
volumes = [
  "./snapshot:/chain:ro",
  "../../tools/token-ledger/tokens.toml:/config/tokens.toml:ro",
  "./spool:/spool:ro",
  "ledger:/db",
]
ports = ["127.0.0.1:8185:8185"]
cpus = 4
memory = "8G"
restart = "on-stop"
max_retries = 10

[[services.ledger.depends_on]]
service = "sieve"
condition = "service_started"

[services.ledger.healthcheck]
exec = ["curl", "-fsS", "http://127.0.0.1:8185/health"]
interval = "30s"
timeout = "5s"
start_period = "30s"
retries = 3
```

Four details in that file are load-bearing and easy to get wrong:

- **`restart` is the M2 form.** `up` refuses anything but `restart = "no"`
  until the supervisor lands, so the first working version of this file sets
  both services to `no` and flips them back in M2.

- **`--listen 0.0.0.0:…`.** Both binaries default to `127.0.0.1`, which inside a
  container is the container's own loopback. A published port forwards to the
  container's *network* address, so a loopback listener is unreachable. The
  host-side publish stays on `127.0.0.1`, so nothing is exposed to the LAN.
- **`spool` is a host bind mount**, read-write for `sieve` and read-only for
  `ledger`. A named volume cannot be attached to two running containers — the
  probe in [`07-build-plan.md`](07-build-plan.md#d2--one-running-container-per-named-volume-forced)
  — and a bind mount is the documented sharing path. The directory must exist
  before `up`; a missing bind source is a load error.
- **`--index-dir` is omitted.** It defaults to `/opt/tx-index/mainnet`, which
  does not exist here; `wallet-sieve` logs a warning per resolve and falls back
  to decode+hash. Correct for a local stack, and the warning is expected.

### What the fixture proves, and what it cannot

It proves the shape: a snapshot-backed service, a spool writer, a spool reader,
two published ports, a dependency and a healthcheck, with no network access
after the snapshot.

It cannot prove **Dust Bunny** yet. At design time (2026-10-24 ~00:40 UTC) the
newest Mithril snapshot (`9313`) ends around 2026-10-23 21:18 UTC, and the
`$BUNNY` launch was 22:50 UTC — inside the lag. The `backbone` spool reaches the
tip, but `token-ledger`'s coverage must start at the chunk tip, which is below
the launch, so the token is not walkable from this snapshot. Wait for a snapshot
whose `created_at` is after the launch before treating the fixture as a test of
LumpFun tracking; until then it tests the plumbing.

## Open questions

Recorded here rather than settled by the design, because each is a decision with
a cost on both sides.

1. **The binary name.** `container-compose` is accurate and long. `ccx`, `cc`,
   `stak` and `up` are all available-sounding. The repository name does not have
   to match the binary. Decide at scaffold time; nothing in the design depends
   on it.
2. **Does `up -d` survive at all?** The design allows it only for stacks with no
   restarts. An alternative is to remove it until launchd integration exists and
   tell people to use the foreground. The cost of keeping it is one refusal
   branch; the cost of removing it is nobody can start a stack from a script.
3. **Should the tool check the host's DNS setup?** `plan` currently warns when
   aliases are declared and no `[dns] domain` is configured. It could go further
   and check `/etc/resolver/test`, but that is a host-configuration audit, not a
   stack plan.
4. **Named builds.** Two services from one build context currently build twice.
   An `[images]` table would fix it, and would also make the mitos fixture’s
   `container build` on the host unnecessary. It is a schema change; do it in
   M3 or not at all.
5. **Locking.** A pid-based lock is in M2. A lock that survives a killed
   supervisor is harder (macOS has no portable pidfd), and a stale lock that
   blocks `up` is worse than two supervisors fighting. If it proves flaky, drop
   it and rely on name collisions.
6. **Remote runtimes.** Out of scope above, but the `--runtime` flag already
   exists for the fake. If remote ever lands, it is `--runtime ssh://…` with a
   path-mapping problem, and the answer is probably "don't".

## What changes when the runtime answers

All ten are answered in
[`04-runtime.md`](04-runtime.md#measured-facts) and the probe detail is in
[`07-build-plan.md`](07-build-plan.md). Three changed this document:

- Labels are in `list --all --format json`, so ownership keeps both its label
  and its name check and the reconcile table is unchanged.
- Concurrent volume attachment is refused, so the fixture's spool is a host bind
  mount and the rule is one service per named volume.
- `start` does restart an exited container with its original argv, so the
  restart action stays a start. It returns before the container is `running`,
  so every start polls.

Everything else was a detail inside the adapter.
