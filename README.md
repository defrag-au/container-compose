# container-compose

A small orchestration layer over Apple's [`container`](https://github.com/apple/container),
for local multi-service stacks on a Mac.

`container` deliberately stops at the primitives — run, create, start, stop,
delete, networks, volumes, labels, JSON inspect, logs, exec — and has no compose.
This tool is the composition layer: one TOML file, a plan you can read before it
runs, ownership by label, and a foreground supervisor because the runtime has no
restart policy.

**Status: M0–M4 built, 2026-10-24.** `config`, `plan`, `up` (foreground and
`-d`), `down`, `ps` and `logs` work against the real runtime, with restart
policies, healthchecks and the health cache.

Start at [`docs/README.md`](docs/README.md); the design is [`docs/01-model.md`](docs/01-model.md)
through [`docs/05-testing.md`](docs/05-testing.md), and the build plan — including
what the runtime actually does — is [`docs/07-build-plan.md`](docs/07-build-plan.md).
