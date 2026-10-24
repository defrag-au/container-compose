# container-compose

A composition layer over Apple's [`container`](https://github.com/apple/container),
for local multi-service stacks on a Mac.

`container` deliberately stops at the primitives — run, create, start, stop,
delete, networks, volumes, labels, JSON inspect, logs, exec — and has no compose.
This is that layer: one TOML file, a plan you can read before anything runs,
ownership carried on labels, and a foreground supervisor, because the runtime has
no restart policy of its own.

It is not a Compose clone and does not read Compose files. Anything the runtime
cannot honour is refused rather than accepted and ignored — see
[`docs/02-stack-file.md`](docs/02-stack-file.md#why-not-compose-yaml). It only
ever stops and deletes what it created: `delete --all`, `volume prune` and
`network prune` are never used.

## A stack, and its plan

```toml
version = 1
name = "minimal"

[services.echo]
image = "docker.io/library/alpine:latest"
command = ["echo", "hello"]
```

```console
$ container-compose plan
PLAN  minimal  config c3431d7551ae4ef5  (/path/to/stack.toml)

  keep      network   default           mode default, subnet 192.168.64.0/24
  create    container minimal-echo      docker.io/library/alpine:latest
  start     container minimal-echo      docker.io/library/alpine:latest

3 steps. Run `container-compose up` to apply.
```

`plan --commands` prints the exact `container` argv instead, and `plan --json`
the same thing as a document.

## Build it

There is no packaged binary yet; the flake exports the devshell, not a package.
From the repo root, in a shell with that devshell loaded:

```sh
direnv allow .            # once per checkout; afterwards `cd` loads it
cargo build --release
./target/release/container-compose --help
```

Without direnv, `nix develop -c cargo build --release` does the same.

## Requirements

- macOS on Apple silicon, with [`container`](https://github.com/apple/container)
  1.6.0 or later running (`container system status`).
- A container DNS domain, if the stack's services name each other — which most
  do.

Register the domain once per machine:

```toml
# ~/.config/container/config.toml
[dns]
domain = "test"
```

```sh
container system stop && container system start
sudo container system dns create test
```

Without it, a service dialling `api.test` fails at its first lookup, which looks
like a broken image rather than missing host setup. `plan` warns when a stack
names a service and no domain is registered. Containers created before the
domain was registered do not get a name — `down` then `up` after setting it.

## Commands

| command | what it does |
|---|---|
| `config` | print the resolved stack, without contacting the runtime |
| `plan` | print what `up` would do, without running anything; `--commands`, `--json` |
| `up` | create and start in dependency order, then supervise until Ctrl-C |
| `down` | stop and delete what this project owns; `--volumes`, `--timeout` |
| `ps` | what is running, with health; `--probe`, `--json` |
| `logs`, `exec`, `run` | the runtime's own, scoped to the stack |
| `build`, `pull` | the images the stack declares |

`up -d` exists, but it refuses a service with a restart policy: a policy that
only worked while a supervisor was attached would be a lie.

## Status

The whole surface above works and is exercised against the runtime. `container`
1.6.0 on macOS 27.0.1 arm64 runs the `midnight-lab` ledger-9 devnet — node,
indexer and prover — with the indexer syncing through container-name DNS and
Ctrl-C stopping all three in reverse order.

The same behaviour is covered without a runtime: 162 tests — most of them
driving `dev/fake-container`, which records every argv — plus one `#[ignore]`d
smoke test that runs a real three-service stack
(`cargo test -p container-compose-cli --test smoke -- --ignored --nocapture`).
CI runs fmt, clippy and the suite on `[self-hosted, netcup-arm]`.

## Docs

Read [`docs/README.md`](docs/README.md) first. The design is
[`01`](docs/01-model.md)–[`05`](docs/05-testing.md);
[`06`](docs/06-roadmap.md) is what is not built yet, and
[`07`](docs/07-build-plan.md) is the record of what the runtime actually does,
including the findings that changed the design.
