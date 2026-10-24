# container-compose — design

Status: **built, 2026-10-24.** `01`–`05` are the contract the code is held to.
`07` is what happened when the runtime was measured: the design changes it forced,
the build order, and the probes taken after the first build. Where a document here
and `07` disagree, `07` is the one that was measured.

Seven documents, written to be read in order and then used as a checklist. The
test of the design is not that it is complete — it is that the build session can
tell, from the document, what to do when the runtime disagrees with it. That
record is the last document.

A small orchestration layer over Apple's
[`container`](https://github.com/apple/container), which runs Linux containers as
lightweight VMs on Apple silicon and deliberately stops at the primitives: run,
create, start, stop, delete, networks, volumes, labels, JSON inspect, logs, exec.
It has no compose. This tool is the composition layer, sized to what the runtime
actually guarantees rather than to what Docker Compose taught everyone to expect.

## The one-paragraph version

A stack is a TOML file naming services, volumes and networks. The tool computes an
ordered **plan**, refuses to start if the plan cannot be executed, creates only the
resources it owns (identified by labels and names), starts services in dependency
order, waits for declared readiness, and — when run in the foreground — supervises
and restarts what the stack says should restart. `down` removes exactly what the
tool created and nothing else. It is a planner, an ownership ledger and a
supervisor; it is not a runtime, not a daemon and not a Compose clone.

## Read in this order

| doc | what it settles |
|---|---|
| [`01-model.md`](01-model.md) | the concepts, ownership, lifecycle, and the supervisor decision |
| [`02-stack-file.md`](02-stack-file.md) | the TOML reference, validation rules and config hashing |
| [`03-cli.md`](03-cli.md) | commands, flags, outputs, exit codes |
| [`04-runtime.md`](04-runtime.md) | what Apple `container` does and does not provide, and the mapping |
| [`05-testing.md`](05-testing.md) | the fake runtime, golden plans, and what needs the real thing |
| [`06-roadmap.md`](06-roadmap.md) | milestones, the first consumer, open questions |
| [`07-build-plan.md`](07-build-plan.md) | measured runtime behaviour, the design deltas it forces, and the build order |

## The three decisions, and where they landed

1. **Own TOML, or Compose-file compatibility?** Own TOML, built. A Compose file
   would promise restart policies, healthchecks, `depends_on` conditions and DNS
   that the runtime does not provide; the features that cannot be honoured are
   refused instead. See
   [`02-stack-file.md`](02-stack-file.md#why-not-compose-yaml).
2. **Foreground supervisor, or launchd?** Foreground, built. `container` has no
   restart policy, so a detached stack cannot heal: `up -d` refuses any service
   that asks to restart. launchd supervision remains on the
   [roadmap](06-roadmap.md) and is not built. See
   [`01-model.md`](01-model.md#the-supervisor-decision).
3. **Shared writable volumes across containers.** Settled by measurement: a host
   bind mount is shareable across running containers, with `:ro` enforced (see
   [`07-build-plan.md`](07-build-plan.md)). Named volumes stay one writer per
   volume. See [`04-runtime.md`](04-runtime.md#volumes).

## Naming

The repository is `container-compose`; the working binary name is
`container-compose`. "Compose" describes the role, not a compatibility promise —
see decision 1. A shorter binary name is an open question, not a blocker.
