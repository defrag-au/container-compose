# 04 — The runtime

Status: measured, 2026-10-24, against `container` 1.6.0 on macOS 27.0.1 (arm64).
The original design was written from upstream documentation; the rows below
carry the measured answer where the probe disagreed with it or settled a
question. The probe's commands and captured JSON are in
[`07-build-plan.md`](07-build-plan.md#m0--the-ten-questions).

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
| start a stopped container | yes | `container start` takes exactly one container, and returns before it is `running`; the supervisor polls `list --all` for that state. `--attach` is not used | command-reference `container start`; measured |
| stop with a grace period | yes | `container stop --time <secs>` (default 5 in the runtime; the tool defaults to 15) | command-reference `container stop` |
| kill | yes | `container kill` on the second SIGINT | command-reference `container kill` |
| delete | yes | `container delete`; `--force` only for a container that would not stop | command-reference `container delete` |
| list, machine-readable | yes | `container list --all --format json` returns the full configuration — labels, env, mounts, networks and aliases, ports, resources and the image digest. One call is enough; per-container `inspect` is not needed | measured |
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
| exit code | only via `start --attach` | not used: the code is available only to the process that starts the container, and attach without a TTY emits a signal-forwarding flood | measured |
| restart policy | **no** | implemented by the foreground supervisor; the runtime records only `running`/`stopped` | no such flag in the command reference; measured |
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

Measured: `list` and `inspect` report only `running` or `stopped` — no `exited`,
no exit code, no finish time — so a policy that tests the exit status cannot be
implemented. `start --attach` does propagate the code (7 for `exit 7`), but only
to the process that starts the container, and a container stopped by
`container stop` returns 137, so it does not distinguish a manual stop from a
crash either. `on-stop` is named for what the tool can observe.

### No healthcheck

There is no `--health-cmd`, no health state in `list`, and no readiness gate.
The tool runs `container exec <name> <argv>` itself, treats a zero exit as
healthy, and keeps the result in its per-project cache for `ps`. Nothing else
consumes it except `depends_on: service_healthy`.

Measured outcomes: exit 0 is healthy; a non-zero exit is a failed probe; `exec`
on a stopped container exits 1 with `container <name> is not running`, which is
`unknown` rather than unhealthy. A missing probe binary also exits 1, with
`failed to find target executable`, and is reported separately because the check
can never pass.

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
`/etc/resolver/test` so macOS does too. Both are host setup; the tool performs
neither. `plan` warns once when one service names another by container name and
no domain is registered — the case where the omission otherwise looks like a
broken service.

Measured on 1.6.0 on 2026-10-24, in the order that matters:

- `sudo container system dns create test` **alone is not enough**. The zone is
  created — `container system dns list` lists it — but every container keeps a
  single-label hostname and nothing resolves, including a container created
  after the zone appeared.
- Adding `[dns] domain = "test"` and restarting the system is what makes a name
  resolvable. After it, both `midnight-lab-node.test` and the bare
  `midnight-lab-node` resolved from a peer container on `default`.
- Registration happens at **create** time. Containers created before the domain
  was set still did not resolve after a restart; deleting and recreating them
  did. `down` then `up`, not a stop and start.

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

Measured: a host port that is already bound fails at `start`, not at `create`
(`Address already in use (errno: 48)`), so `plan` can only catch duplicates
inside the stack.

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

Measured: an alias containing a dot is accepted with no `[dns] domain`
configured, and aliases are stored and reported on a custom network. A
single-label alias still needs the configured domain, and bare-hostname DNS on a
custom network remains apple/container#1809.

## Volumes

Named volumes are ext4 filesystems on a sparse image, 512 GiB by default. They
are created explicitly (`container volume create`) or implicitly by naming one
in `-v`. This tool always creates explicitly, so the label exists before the
first container does.

Volumes can be sized and journaled at creation only. `sizeInBytes` in `inspect`
is the capacity, not the occupancy. `volume prune` deletes data immediately and
irrecoverably — the tool never calls it. A volume in use by a container,
running or stopped, cannot be deleted.

**Sharing a named volume between running containers is refused.** Measured: a
second container can be created with the mount, but `start` fails with
`The storage device attachment is invalid` while the first runs, even for `:ro`.
A stopped holder releases the volume, which is no use to a stack that starts
together. The tool therefore allows one service per declared volume; sharing
data between services is a host bind mount (which `docs/volumes.md` does
describe as shareable) or one container running both processes.

## Measured facts

All ten questions the design left for the build session are answered; the probe
commands and the captured JSON are in
[`07-build-plan.md`](07-build-plan.md#m0--the-ten-questions). The answers that
changed something:

| # | answer |
|---|---|
| 1 | `list --all --format json` returns the full configuration — labels, env, mounts, networks and aliases, ports, resources and the image digest. One call replaces per-container `inspect`. |
| 2 | `start` restarts an exited container with its original argv — and returns before it is `running`; poll `list --all`. |
| 3 | `logs` returns a stopped container's stored output. |
| 4 | Domain errors are exit 1 with `Error: …` on stderr; usage errors are 64. A bound port fails at `start`. |
| 5 | A second container cannot attach a volume a running container holds, even `:ro`; a stopped holder releases it. |
| 6 | `configuration.image.descriptor.digest` carries the manifest digest, also in `list`. |
| 7 | Aliases are accepted and stored on a custom network; bare-hostname DNS remains #1809. |
| 8 | `stop` on an already-stopped container exits 0. |
| 9 | `delete --force` returns with the container gone and the network reference released. |
| 10 | `system version` omits the apiserver row when it is down; `system status --format json` is the gate. |

Every fixture used by tests is copied from a real run, never written from the
docs. The adapter keeps the raw JSON under `--verbose`.

Two more, from porting a real stack on 2026-10-24 (`container` 1.6.0, macOS
27.0.1 arm64):

- **Container-name DNS is off by default.** With `container system dns list`
  empty — no `~/.config/container/config.toml`, and an empty `[dns]` in
  `container system property list` — neither `<name>.test` nor the bare
  `<name>` resolved, from a container on `default` or from one on a custom
  network with a qualified alias. `container system dns create test` refuses
  without an administrator (`cannot create domain (try sudo?)`). A container's
  IP works. A stack that dials a service by name therefore depends on host
  setup described above.
- **A single-string `Cmd` is not run through a shell.** Each element of the
  image's `Cmd` is exec'd literally, so an image whose `Cmd` is
  `["/nix/store/…/midnight-proof-server --port $PORT"]` fails at start with
  `failed to find target executable /nix/store/…/midnight-proof-server --port $PORT`.
  `start` accepts a `Cmd` override, so the stack spells the shell out:
  `command = ["/bin/sh", "-c", "midnight-proof-server --port $PORT"]`.

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
