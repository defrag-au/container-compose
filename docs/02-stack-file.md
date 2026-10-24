# 02 — The stack file

Status: design, 2026-10-24.

One `stack.toml` per project. It is the whole input: no `.env` discovery beyond
what the file names, no implicit Compose naming, no second file merged in.

The parser and the validator are the product here. `plan` is a pure function of
the file plus the runtime's observed state, and a file that does not validate
never reaches the runtime.

## A complete example

```toml
version = 1
name = "mitos"

[network]
mode = "default"                 # "default" | "isolated"

[volumes.spool]
size = "200G"
journal = "ordered"

[volumes.cache]
size = "20G"

[volumes.ledger]
size = "100G"

[services.sieve]
image = "ghcr.io/defrag/wallet-sieve:0.4.0"
command = ["wallet-sieve", "serve", "--db", "/cache/sieve.db", "--immutable", "/chain/db/immutable"]
env = { RUST_LOG = "info,wallet_sieve=debug" }
volumes = ["spool:/spool", "cache:/cache", "./fixtures:/fixtures:ro"]
ports = ["127.0.0.1:8184:8184"]
aliases = ["sieve"]
cpus = 4
memory = "6G"
ulimits = ["nofile=65536:65536"]
init = true
restart = "always"

[services.sieve.healthcheck]
exec = ["curl", "-fsS", "http://127.0.0.1:8184/health"]
interval = "15s"
timeout = "5s"
start_period = "60s"
retries = 4

[services.ledger]
image = "ghcr.io/defrag/token-ledger:0.4.0"
command = ["token-ledger", "serve", "--data-dir", "/chain", "--tail-db", "/spool/tail.db"]
env = { RUST_LOG = "info" }
volumes = ["spool:/spool:ro", "ledger:/db"]
restart = "on-failure"
max_retries = 10

[[services.ledger.depends_on]]
service = "sieve"
condition = "service_healthy"
```

## Top level

| key | type | default | meaning |
|---|---|---|---|
| `version` | integer | required | Schema version. Only `1` is accepted. |
| `name` | string | directory name | Project name; see the naming rule in [`01-model.md`](01-model.md#naming). |
| `[network]` | table | absent | Network selection; absent means the runtime's `default` network. |
| `[volumes]` | table of tables | `{}` | Named volumes this project owns. |
| `[services]` | table of tables | required, non-empty | One container per service. |

`version` exists so the file can change shape later without guessing. An unknown
version is an error that names the versions this binary understands — never a
best-effort parse.

## `[network]`

| key | type | default | meaning |
|---|---|---|---|
| `mode` | `"default"` or `"isolated"` | `"default"` | Attach to the runtime's `default` network, or create `<project>-net`. |
| `subnet` | CIDR string | runtime-allocated | IPv4 subnet for `isolated`. Not valid with `mode = "default"`. |

`default` is the default because it is the only network where container names
resolve: `container` documents `<name>.test` lookups for containers on the
`default` network, and documents that bare-hostname lookups on a custom network
do not work yet (apple/container#1809). An `isolated` stack is isolated, but
services inside it reach each other by IP or published port — `up` prints that
constraint once rather than letting a service fail its first DNS lookup.

Aliases (`services.<name>.aliases`) are passed as `--network ...,alias=<value>`
and work on either mode to the extent the runtime supports them. An alias
without a dot gets the configured DNS domain, exactly as the runtime documents.

## `[volumes.<name>]`

| key | type | meaning |
|---|---|---|
| `size` | size string | Volume image size, fixed at creation. Defaults to the runtime's 512 GiB sparse image. |
| `journal` | `ordered` \| `writeback` \| `journal`, optionally `:size` | ext4 journal mode, passed as `--opt journal=`. |

A declared volume is created with `container volume create --label … --opt …`.
A volume's size and journal are creation-time properties: changing either is
drift the tool reports but cannot apply without deleting the volume. `up` never
deletes a volume to apply a size change; it prints the mismatch and exits 3.

Anonymous volumes are not supported. `container run -v /path` creates one, and
the runtime does not delete it when the container goes — an unowned, unreclaimed
volume per `up` is not a lifecycle.

## `[services.<name>]`

### Image or build

| key | type | meaning |
|---|---|---|
| `image` | string | Image reference. Mutually exclusive with `build`. |
| `build` | table | Build from a local context instead of pulling. |

```toml
[services.api.build]
context = "."                 # relative to the stack file
containerfile = "Containerfile"
target = "runtime"
args = { FEATURES = "native" }
```

A built service is tagged `<project>-<service>:local`. That name is deterministic
so the config hash does not depend on a timestamp, and it is local so the tool
never pushes anything.

| key | type | default |
|---|---|---|
| `context` | string | `"."` |
| `containerfile` | string | `"Containerfile"` |
| `target` | string | none |
| `args` | table of strings | `{}` |

`pull` controls the image pull policy:

| value | behaviour |
|---|---|
| `"missing"` (default) | pull only when the image is absent locally. |
| `"always"` | always `container image pull` before create. |
| `"never"` | never pull; an absent image is an error. |

### Process

| key | type | meaning |
|---|---|---|
| `command` | array of strings | Overrides the image's command. Never a shell string. |
| `entrypoint` | array of strings | Overrides the image's entrypoint. Never a shell string. |
| `env` | table of strings | Environment variables. Values must be strings. |
| `env_file` | array of strings | Files of `KEY=value`, resolved relative to the stack file. Later files win; `env` wins over every file. |
| `user` | string | `uid[:gid]` or `name[:group]`, as `container` accepts. |
| `workdir` | string | Working directory. |
| `init` | bool | Run the runtime's init process (`--init`). |
| `shm_size` | size string | `/dev/shm` size. |
| `cap_add` / `cap_drop` | array of strings | Linux capabilities, passed through. |
| `ulimits` | array of strings | `<type>=<soft>[:<hard>]`, passed through verbatim. |

`command` and `entrypoint` are argv arrays, not strings. A string would invite
`sh -c`, and `sh -c` turns a quoting bug into a different program. If a service
needs a shell, it writes `["/bin/sh", "-c", "..."]` and the choice is visible.

`env_file` entries are read and merged by the tool, then passed as `--env
KEY=value` rather than `--env-file`. That makes the precedence a rule the tool
enforces instead of a rule the runtime does not document, and it keeps
`plan --commands` honest. The cost is that a value from the file appears in the
argv, visible to `ps` on the same machine; a local dev stack is not a secret
store, and a production secret does not belong in one.

### Storage

| key | type | meaning |
|---|---|---|
| `volumes` | array of strings | `name:/path` for a declared volume, `./host:/path` or `/abs:/path` for a bind mount. Optional `:ro`. |
| `tmpfs` | array of strings | `/path` or `/path:size=64M,mode=1777`, passed as `--tmpfs`. |
| `read_only` | bool | Mount the root filesystem read-only. |

Relative bind paths resolve against the directory holding `stack.toml`, not the
shell's working directory, so `cd` cannot change what a stack means. A bind
source that does not exist is a load error.

A volume used by more than one service must be `:ro` on all but one of them.
The runtime documents mounting and does not document concurrent attachment; see
[`01-model.md`](01-model.md#constraints-the-model-accepts).

### Network

| key | type | meaning |
|---|---|---|
| `ports` | array of strings | `[host-ip:][host-port:]container-port[/tcp\|/udp]`, passed as `--publish`. |
| `aliases` | array of strings | Extra DNS names, passed as `alias=` on the network attachment. |

`plan` detects two services publishing the same host address and port. A
conflict with a container outside the stack is the runtime's error to report;
the tool adds the failing publish spec to its message.

### Dependencies and lifecycle

| key | type | default | meaning |
|---|---|---|---|
| `depends_on` | array of tables | `[]` | Ordering constraints. |
| `restart` | `"no"` \| `"on-failure"` \| `"always"` | `"no"` | Tool-level restart policy. |
| `max_retries` | integer | `5` | Only for `on-failure`. |
| `healthcheck` | table | absent | Probe run with `container exec`. |
| `labels` | table of strings | `{}` | Extra labels, merged with the tool's. |

```toml
[[services.api.depends_on]]
service = "db"
condition = "service_started"     # or "service_healthy"
```

- A dependency must name a declared service; self-dependency and cycles are load
  errors.
- `service_healthy` requires the dependency to declare a healthcheck.
- Dependencies order **start**; they do not order stop. `down` and Ctrl-C stop
  in the reverse of the start order.
- A dependency does not imply a restart when the dependency restarts.

`healthcheck`:

| key | type | default |
|---|---|---|
| `exec` | non-empty array of strings | required |
| `interval` | duration | `"30s"` |
| `timeout` | duration | `"5s"` |
| `start_period` | duration | `"0s"` |
| `retries` | integer | `3` |

There is no `cmd` string and no `HEALTHCHECK` image directive. The runtime has no
healthcheck, the tool runs `container exec`, and an argv is the only form of
that which cannot be mangled by a shell.

`restart = "always"` with `up -d` is refused — see
[`01-model.md`](01-model.md#the-supervisor-decision).

## Validation

Every rule below fails the load; none of them is a warning except where stated.

| rule | error identifies |
|---|---|
| `version` present and equal to `1` | `version` |
| unknown keys anywhere | the full key path, with a did-you-mean when close |
| exactly one of `image` / `build` | `[services.<name>]` |
| at least one service | `[services]` |
| project and service names match `[a-z0-9][a-z0-9-]{0,30}` | the name |
| `depends_on` names exist, no self, no cycle | `[services.<name>.depends_on]` |
| `service_healthy` only where a healthcheck exists | the dependency entry |
| every `volumes` source is either a declared volume or an existing host path | the entry |
| at most one writer per declared volume | the second entry |
| `size` / `memory` parse as sizes; `cpus > 0`; `retries >= 1` | the key |
| `ports` parse; no duplicate host bindings inside the stack | the entry |
| `env_file` exists; `env` keys match `[A-Za-z_][A-Za-z0-9_]*` | the file or key |
| `labels` do not start with `dev.container-compose.` | the label |
| `subnet` only with `mode = "isolated"` | `[network]` |
| durations parse as `ms`, `s`, `m`, `h` | the key |

Errors name a **key path**, not a byte offset: `[services.sieve].envs`. A line
number is a nice-to-have if the TOML parser of choice carries spans; the design
does not depend on it, and a test asserts the key path is in every message.

## The config hash

`up` compares a container's `dev.container-compose.config` label with the hash of
the resolved service. The hash is:

1. the effective service model — image or build tag, command, entrypoint, env
   **after** file and override resolution, volumes with absolute host paths,
   ports, aliases, network mode, cpus, memory, ulimits, user, workdir, init,
   tmpfs, shm size, read-only, capabilities, restart policy, healthcheck,
   user labels;
2. rendered as JSON with object keys sorted and arrays left in declared order —
   `command` and `volumes` are ordered, so sorting them would hide a change;
3. hashed with SHA-256, truncated to 16 hex characters.

Excluded on purpose: `depends_on` (ordering is not container configuration),
the project name (it is already a label), the file's comments and whitespace.

Including env values means a changed `.env` recreates the container, which is
the behaviour people expect and the reason the hash is not taken over the file
text. The label holds a digest, not the values; a secret in `env` or `env_file`
is not printed by `plan`, `ps` or `up`.

Volume `size` and `journal` are **not** part of the container hash — they belong
to the volume, and a mismatch is reported separately by the volume reconciler
because it cannot be fixed without deleting data.

## Why not Compose YAML?

Because most of a Compose file would be silently dropped, and a file that parses
but does not mean what it says is worse than a file that is refused.

What a Compose translation could not honour on Apple `container`:

| Compose feature | what would happen |
|---|---|
| `restart: always` | has no runtime equivalent; it would work only under the foreground supervisor, so a detached stack would silently not restart. |
| `healthcheck:` | no runtime healthcheck; the tool can only approximate it with `exec`. |
| `depends_on: {condition: service_completed_successfully}` | needs exit-code history the runtime does not keep. |
| `deploy.replicas` / `scale` | no equivalent; would be ignored or faked with names. |
| `secrets:` | no secret store; would fall back to env or files. |
| bare service-name DNS on a user-defined network | does not work (apple/container#1809), so the Compose habit would fail at runtime. |
| `network_mode: host`, `pid: host`, `ipc: host` | no equivalent. |
| `extends`, `profiles`, YAML anchors | support would exist in the parser and not in the model, which is the definition of a lie. |

The reverse is also true: a Compose file would let people bring their habits
unchanged and then blame the runtime for the gap. An own format of a few dozen
keys costs one afternoon to learn and cannot promise something it does not
deliver.

The tool does not read `docker-compose.yml`, does not emit one, and does not
claim compatibility. The name `container-compose` describes the role.
