# 04 — The runtime

Status: design, 2026-10-24. **Apple `container` was not installed on the design
machine**, so every row below is sourced from upstream documentation, not from a
local run. Rows marked **verify** are the ones the build session must confirm on
a real runtime before they are relied on.

Sources are the `main`-branch docs (the command reference carries a warning that
it documents the current branch, not a release):

- <https://github.com/apple/container/blob/main/docs/command-reference.md>
- <https://github.com/apple/container/blob/main/docs/networking.md>
- <https://github.com/apple/container/blob/main/docs/volumes.md>
- <https://github.com/apple/container/blob/main/README.md>

## What the runtime is

`container` runs Linux containers as lightweight VMs on Apple silicon. It
requires macOS 26 and an Apple-silicon Mac; the README states older macOS is
unsupported. It consumes and produces OCI images, so images can come from any
standard registry.

Two consequences matter here:

- **A container is a VM.** Boot is not free — the tool's readiness waits and its
  poll interval are sized for hundreds of milliseconds, not microseconds.
- **There is an apiserver but no container daemon.** `container system start`
  registers launchd services, including `container-apiserver` and a DNS service.
  Nothing in it implements a restart policy for your containers.

`container` is product-versioned, not semver: "1.2.3" is a product version, and
the CLI preserves compatibility within a major release. The tool records
`container system version` at `up` time and refuses (exit 5) when the output
cannot be parsed, rather than guessing at a version.

## Capability matrix

| capability | available? | mapping in this tool | source |
|---|---|---|---|
| create a container without starting it | yes | `container create` — the core of `up`, so config can be written before any process starts | command-reference `container create` |
| start a stopped container | yes | `container start`; `--attach`/`--interactive` are not used by the supervisor | command-reference `container start` |
| stop with a grace period | yes | `container stop --time <secs>` (default 5 in the runtime; the tool defaults to 15) | command-reference `container stop` |
| kill | yes | `container kill` on the second SIGINT | command-reference `container kill` |
| delete | yes | `container delete`; `--force` only for a container that would not stop | command-reference `container delete` |
| list, machine-readable | yes | `container list --all --format json` | command-reference `container list` |
| inspect a container | yes, JSON | `container inspect <name>` | command-reference `container inspect` |
| logs | yes | `container logs [-f] [-n N]` | command-reference `container logs` |
| exec | yes | `container exec`; healthchecks and `exec` command | command-reference `container exec` |
| copy files | yes | not used in v1; available for a future `cp` | command-reference `container copy` |
| named volumes | yes | `container volume create --label … --opt size=… --opt journal=…` | command-reference `container volume create`; `docs/volumes.md` |
| volume size | yes, creation-time | `--opt size=` or `-s`; `-s` wins if both are given | command-reference `container volume create` |
| volume journal mode | yes, creation-time | `--opt journal=ordered\|writeback\|journal[:size]` | `docs/volumes.md` |
| sparse volume image | yes | 512 GiB default capacity, sparse on disk | `docs/volumes.md` |
| bind mounts | yes | `-v /host:/container[:ro]` | `docs/volumes.md` |
| tmpfs | yes | `--tmpfs /path[:size=…,mode=…]` | `docs/volumes.md` |
| user networks | yes, macOS 26+ | `container network create [--subnet] [--label]` | command-reference `container network create` |
| network aliases | yes | `--network name,alias=NAME` (repeatable) | command-reference `container run`; `docs/networking.md` |
| publish ports | yes, loopback | `-p [host-ip:]host-port:container-port[/proto]` | `docs/networking.md` |
| resource limits | yes | `--cpus`, `--memory` (MiB granularity, K/M/G/T suffixes) | command-reference `container run` |
| ulimits | yes | `--ulimit type=soft[:hard]` | command-reference `container run` |
| init process | yes | `--init` | command-reference `container run` |
| read-only rootfs | yes | `--read-only` | command-reference `container run` |
| capabilities | yes | `--cap-add` / `--cap-drop` | command-reference `container run` |
| labels on containers | yes | `-l/--label key=value` | command-reference `container run` |
| labels on volumes and networks | yes | `volume create --label`, `network create --label` | command-reference |
| image pull | yes | `container image pull` | command-reference `container image pull` |
| image build | yes, BuildKit | `container build -t … -f … --target … --build-arg …` | command-reference `container build` |
| registry auth | yes | out of scope; the user runs `container registry login` | command-reference `container registry login` |
| restart policy | **no** | implemented by the foreground supervisor | no such flag in the command reference |
| healthcheck | **no** | `container exec` on a schedule | no such flag in the command reference |
| readiness / `--wait` | **no** | the supervisor polls `list` and runs the probe | no such flag in the command reference |
| compose / multi-service file | **no** | this tool | the command reference has no `compose` |
| container name resolution, default network | yes, with setup | see DNS below | `docs/networking.md` |
| container name resolution, custom network | **bare hostname: no** | not relied on | `docs/networking.md`, apple/container#1809 |
| `--all` / `prune` | yes | **deliberately unused** by this tool | command-reference |
| `container machine` | yes | out of scope — a persistent VM, not a service container | command-reference `container machine` |
| `container cluster` | experimental | out of scope | command-reference `container cluster` |

## Concepts the runtime does not have

These are the gaps that shape the whole design, so they are stated once here
rather than repeated per command.

### No restart policy

There is no `--restart` flag on `create` or `run`, and no daemon that could hold
one. A container that exits stays exited until something starts it. The
apiserver is a control plane, not an init.

Therefore `restart` is a property of the **tool**, honoured only while the
supervisor is running. A detached stack cannot heal, which is why
[`03-cli.md`](03-cli.md#up) refuses `up -d` when a service asks for restarts.

### No healthcheck

There is no `--health-cmd`, no health state in `list`, and no readiness gate.
The tool runs `container exec <name> <argv>` itself, treats a zero exit as
healthy, and keeps the result in memory. `ps` shows it; nothing else consumes it
except `depends_on: service_healthy`.

Consequences worth knowing: a probe is a process start inside the VM, so
`interval` below a couple of seconds on a heavy image is a bad idea; and a probe
cannot run before the container is running, so `start_period` exists.

### No compose, no dependency graph, no project label

There is no file format, no grouping primitive, and no label convention beyond
the labels you pass. Everything about projects, ordering and ownership is this
tool's invention, written down in [`01-model.md`](01-model.md#ownership).

## Networking

The default network is created by `container system start` (`192.168.64.0/24`
in the documentation's example). Containers get an IP reachable from the host
and from other containers on the same network.

### DNS is a two-step setup

For `<name>.test` to resolve:

```toml
# ~/.config/container/config.toml
[dns]
domain = "test"
```

```sh
container system stop && container system start
sudo container system dns create test
```

The first step registers every container as `<name>.test` **inside** the
runtime's DNS and configures containers to use it. The second writes
`/etc/resolver/test` so macOS does too. Both are host setup; the tool does not
perform either, but `plan` warns when the domain is unset and a stack declares
aliases.

### The custom-network gap

The documentation is explicit:

> This works for containers on the `default` network using a domain-qualified
> name (`http-server.test`, as above). It does **not** currently work for
> looking up another container by its *bare* hostname (no domain suffix) on a
> custom network created with `container network create` — the kind of
> zero-configuration, Compose-style service discovery some users expect. That
> gap is tracked upstream as apple/container#1809.

So the design defaults to the `default` network, and an `isolated` stack is
documented as IP-or-published-port only. Do not build service discovery on a
custom network until #1809 is closed and a release ships it.

### Ports are loopback

`--publish` forwards from the loopback IP (`127.0.0.1` or `::1`), which is the
right default for a local dev stack: nothing is exposed to the LAN unless a
`host-ip` is given explicitly. When a container is attached to several networks,
the publish forwards to the interface on the **first** network attachment — so
the tool puts the stack network first and never relies on a second attachment
for a published port.

### Aliases

`--network <name>,alias=api,alias=.apps` gives extra DNS names. The rules the
docs enforce and the tool must therefore validate or pass through untouched:

- an exact alias needs at least two labels, unless it has no dot and gets the
  default domain;
- a leading-dot alias must be below a name the same attachment owns;
- every exact name is unique across all containers, running or stopped;
- at most 32 aliases per network;
- aliases are fixed at container creation — changing one is a config-hash
  change and therefore a recreate;
- `*` is rejected; the leading-dot form is the subtree form.

## Volumes

Named volumes are ext4 filesystems on a sparse image, 512 GiB by default. They
are created explicitly (`container volume create`) or implicitly by naming one
in `-v`. This tool always creates explicitly, so the label exists before the
first container does.

Volumes can be sized and journaled at creation only. `sizeInBytes` in `inspect`
is the capacity, not the occupancy. `volume prune` deletes data immediately and
irrecoverably — the tool never calls it. A volume in use by a container,
running or stopped, cannot be deleted.

**Sharing a volume between containers is undocumented.** `docs/volumes.md` says
what a volume is and how to mount it, and says nothing about two containers
attaching the same one. The design allows at most one writer and marks
read-only sharing as **verify** on a real runtime. If it turns out concurrent
attachment is unsupported, the fallback is one container running both
processes, or a bind mount — both already expressible in the stack file.

## Facts to confirm in the build session

Each of these is cheap to test on a real runtime and would change a detail if it
came out the other way. They are listed so the build session tests them
deliberately rather than discovering them in production.

| # | question | why it matters |
|---|---|---|
| 1 | Where do labels appear in `container inspect`'s JSON, and what is the exact shape of `list --format json`? | Ownership filtering and config-hash comparison; pin a fixture from a real run. |
| 2 | Does `container start` on an exited container restart it with its original argv? | The supervisor's restart path. |
| 3 | Does `container logs` return the stored log of a stopped container? | `down` followed by `logs` is a common debugging path. |
| 4 | What is `container`'s exit code and stderr shape for "no such container", "name in use", "port already bound"? | Distinguishing exit 1 from exit 3 in the tool. |
| 5 | Can two containers mount the same named volume, read-only and read-write? | The one-writer rule. |
| 6 | Does `container image inspect` expose a manifest digest? | Drift reporting when a tag moves. |
| 7 | Does `--network name,alias=…` work on a custom network? | Whether `isolated` can ever offer names. |
| 8 | Does `container stop` exit non-zero when the container is already stopped? | Idempotent `down`. |
| 9 | Does `container delete --force` on a running container return before the VM is gone? | Ordering `down` correctly. |
| 10 | What does `container system version` print as JSON, and what does it do when the apiserver is down? | The exit-5 gate. |

Until 1–4 are answered, the runtime adapter keeps the raw JSON in a debug log
(`--verbose`) and every fixture used by tests is copied from a real run, never
written from the docs.

## Minimum surface the tool requires

Always: `create`, `start`, `stop`, `delete`, `list --format json`, `inspect`,
`logs`, `exec`, `image pull`, `image inspect`, `volume create`, `volume delete`,
`volume inspect`, `system version`.

Only for `network.mode = "isolated"`: `network create`, `network delete`,
`network inspect`.

All are in the command reference, and none is marked experimental except the
Kubernetes `cluster` commands, which are not used.

If a required subcommand is missing, `up` exits 5 with it named. The tool does
not attempt a fallback — a partially-capable runtime produces a half-created
stack, which is exactly the state the design works to avoid. A stack that only
uses the `default` network therefore runs on a runtime without the network
commands at all.
