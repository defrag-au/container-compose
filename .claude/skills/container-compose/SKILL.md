---
name: container-compose
description: Use when running, inspecting or porting a multi-service local stack on Apple's `container` runtime — planning a stack, bringing one up in the foreground, reading its health and logs, or moving a docker-compose file onto it.
---

# Running a stack with container-compose

`container-compose` is the composition layer for Apple's `container` runtime. It
reads `stack.toml`, **not** compose YAML. Do not translate a compose file on the
fly: the tool refuses what the runtime cannot honour, and a silent translation is
the failure it exists to prevent.

## 0. Is it there?

```sh
container-compose --help        # from a defrag devshell (the `container-tooling` group)
```

Missing from the shell means defrag-nix's `container-compose` input has not been
realised yet. Build from a checkout rather than reaching for a global install —
the repository's flake exports the devshell, not a package:

```sh
direnv allow . && cargo build --release    # ./target/release/container-compose
```

## 1. Plan before starting

```sh
container-compose -f stack.toml plan              # the steps
container-compose -f stack.toml plan --commands   # the exact `container` argv
```

- Exit `3` means refused and nothing ran. The refusal names a key path
  (`[services.api].ports`), not a line number.
- A `warn` row mentioning `[dns] domain` means container names will not resolve —
  see step 3.

## 2. What `up` actually does

```sh
container-compose -f stack.toml up
```

- It runs in the **foreground** whenever any service has a restart policy, and
  `-d` is refused in that case by design. A restart policy that only worked while
  a supervisor was attached would be a lie.
- `-d` is allowed only when every service is `restart = "no"`.
- `Ctrl-C` stops services in reverse dependency order; a second `Ctrl-C` kills.
- Exit codes: `0` ok · `1` runtime failure · `2` invalid stack · `3` refused ·
  `4` readiness · `5` runtime unavailable · `130` interrupted.

## 3. The DNS prerequisite — measured, not optional

Without a registered domain, a service dialling `api.test` fails its first lookup
and it reads as a broken image. The setup is host-level, once per machine, and
the **order matters**:

```toml
# ~/.config/container/config.toml
[dns]
domain = "test"
```

```sh
container system stop && container system start
sudo container system dns create test
```

`container system dns list` alone is not enough, and containers created before
the domain existed never register a name — `down` then `up` after setting it.
`plan` warns when a stack names a service and no domain is registered.

## 4. Check it; do not infer it from a clean `up`

```sh
container-compose ps --probe
container-compose logs -n 50 <service>
container-compose logs --follow <service>
```

Then query the service itself — its RPC, its GraphQL endpoint, its `/health`.
`ps` showing `unknown` health means no healthcheck is declared, not that the
service is broken. Never report "the stack works" from `up` returning.

## 5. Porting a docker-compose file

| compose | container-compose |
|---|---|
| `container_name: x` | `<project>-<service>` (matching an explicit name keeps scripts working) |
| `ws://node:9944` | `ws://<project>-node.test:9944` |
| `"9944:9944"` | `"127.0.0.1:9944:9944"` — loopback is deliberate, not a typo |
| `depends_on: service_started` | `condition = "service_healthy"` plus a healthcheck |
| `restart: on-failure` | `restart = "on-stop"` + `max_retries` |
| `restart: unless-stopped` | `restart = "always"` |
| `environment:` | `[services.x.env]`; an inline table cannot hold a value with `{}` |
| `env_file`, `extends`, `profiles`, `scale`, `network_mode: host` | refused |

Two traps when writing the TOML:

- Every `[services.x]` key must come **before** the first `[services.x.*]`
  subtable; re-opening the parent is a duplicate-key error.
- An image whose `Cmd` is Docker shell-form (one string,
  `"/nix/store/…/server --port $PORT"`) is exec'd literally and fails at start.
  Override it: `command = ["/bin/sh", "-c", "server --port $PORT"]`.

## 6. Stopping, and what is never done

```sh
container-compose down              # stops and deletes what this project owns
container-compose down --volumes    # also the named volumes — data loss
```

`down` also after registering a DNS domain, so containers are recreated. Never
`container delete --all`, `volume prune` or `network prune`: they reach other
people's work and the tooling deliberately does not use them.

## 7. A service that will not start

- `logs` reads a stopped container, so start there.
- A config change is a recreate: `up` stops, deletes and recreates; state in named
  volumes survives, state in the container's writable layer does not.
  `--no-recreate` refuses instead (exit `3`).
- Restarts print `restart <service> attempt N in Ds`; the counter resets after
  60 s of healthy uptime, so a service that runs for minutes always starts at 1.
- Reaching another container by bare name on a custom (`isolated`) network does
  not work (apple/container#1809). Use the published port or the IP.
