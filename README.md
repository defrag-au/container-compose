# container-compose

A small orchestration layer over Apple's [`container`](https://github.com/apple/container),
for local multi-service stacks on a Mac.

`container` deliberately stops at the primitives — run, create, start, stop,
delete, networks, volumes, labels, JSON inspect, logs, exec — and has no compose.
This tool is the composition layer: one TOML file, a plan you can read before it
runs, ownership by label, and a foreground supervisor because the runtime has no
restart policy.

**Status: design, 2026-10-24. Nothing is built yet.**

Start at [`docs/README.md`](docs/README.md); the implementation brief is
[`docs/06-roadmap.md`](docs/06-roadmap.md).
