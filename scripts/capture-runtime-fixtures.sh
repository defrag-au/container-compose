#!/usr/bin/env bash
#
# Capture the real runtime's JSON into fixtures/real/.
#
# These files are the record `crates/runtime/src/observe.rs` is written from, and
# `crates/runtime/tests/captured.rs` parses them: a field the adapter depends on
# that the runtime no longer sends fails a test, not a stack.
#
# This script touches the real runtime. It creates one project — a network, a
# volume and two containers, all named "$PROJECT-…" and labelled with that
# project — and deletes exactly those names in a trap. It never runs a
# destructive `--all` and never prunes.
#
# Usage:  scripts/capture-runtime-fixtures.sh [output-dir]
#         CONTAINER=/path/to/container scripts/capture-runtime-fixtures.sh
#
set -euo pipefail

CONTAINER="${CONTAINER:-container}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/fixtures/real}"
PROJECT="ccfixtures-$$"

API="$PROJECT-api"
WORKER="$PROJECT-worker"
VOLUME="$PROJECT-vol"
NETWORK="$PROJECT-net"

PROJECT_LABEL="dev.container-compose.project=$PROJECT"
SERVICE_LABEL="dev.container-compose.service"
SCHEMA_LABEL="dev.container-compose.schema=1"
CONFIG_LABEL="dev.container-compose.config=capture"

die() {
    printf 'capture: %s\n' "$*" >&2
    exit 1
}

running_state() {
    # The runtime prints pretty JSON, but match either spacing so a
    # minified response does not silently read as "not running".
    grep -Eq '"state"[[:space:]]*:[[:space:]]*"running"'
}

cleanup() {
    # `stop` on a stopped container exits 0, so only the delete needs guarding.
    "$CONTAINER" delete --force "$API" >/dev/null 2>&1 || true
    "$CONTAINER" delete --force "$WORKER" >/dev/null 2>&1 || true
    "$CONTAINER" volume delete "$VOLUME" >/dev/null 2>&1 || true
    "$CONTAINER" network delete "$NETWORK" >/dev/null 2>&1 || true
}
trap cleanup EXIT

command -v "$CONTAINER" >/dev/null 2>&1 || die "no \`$CONTAINER\` on PATH"
"$CONTAINER" system status --format json | grep -Eq '"status"[[:space:]]*:[[:space:]]*"running"' ||
    die "the container runtime is not running; run \`container system start\`"

"$CONTAINER" image list --format json | grep -q 'docker.io/library/alpine:latest' ||
    die "docker.io/library/alpine:latest is not present; run \`container image pull docker.io/library/alpine:latest\`"

mkdir -p "$OUT"

# A project name that no other run can collide with, so the cleanup trap is
# always scoped to what this invocation created.
"$CONTAINER" network create --label "$PROJECT_LABEL" "$NETWORK" >/dev/null
"$CONTAINER" volume create --label "$PROJECT_LABEL" --opt size=1G "$VOLUME" >/dev/null

"$CONTAINER" create --name "$API" \
    --label "$PROJECT_LABEL" --label "$SERVICE_LABEL=api" \
    --label "$SCHEMA_LABEL" --label "$CONFIG_LABEL" \
    --init --network "$NETWORK,alias=api.$PROJECT.test" \
    --volume "$VOLUME:/data" \
    --env CAPTURE_MARKER=present \
    docker.io/library/alpine:latest /bin/sh -c 'echo capture-hello; exec sleep 120' >/dev/null

# Created and left stopped: the two-state vocabulary, and what a service that
# was never started looks like in `list`.
"$CONTAINER" create --name "$WORKER" \
    --label "$PROJECT_LABEL" --label "$SERVICE_LABEL=worker" \
    --label "$SCHEMA_LABEL" --label "$CONFIG_LABEL" \
    --network "$NETWORK,alias=worker.$PROJECT.test" \
    docker.io/library/alpine:latest /bin/sh -c 'echo worker-done' >/dev/null

# `start` returns before the container is running, so poll rather than sleep.
"$CONTAINER" start "$API" >/dev/null
started=no
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if "$CONTAINER" inspect "$API" 2>/dev/null | running_state; then
        started=yes
        break
    fi
    sleep 1
done
[ "$started" = yes ] || die "$API did not reach running within 10s"

"$CONTAINER" system status --format json >"$OUT/system-status.json"
"$CONTAINER" system version --format json >"$OUT/system-version.json"
"$CONTAINER" system dns list --format json >"$OUT/dns-list.json"
"$CONTAINER" list --all --format json >"$OUT/container-list.json"
"$CONTAINER" inspect "$API" "$WORKER" >"$OUT/container-inspect.json"
"$CONTAINER" volume list --format json >"$OUT/volume-list.json"
"$CONTAINER" volume inspect "$VOLUME" >"$OUT/volume-inspect.json"
"$CONTAINER" network list --format json >"$OUT/network-list.json"
"$CONTAINER" image list --format json >"$OUT/image-list.json"
"$CONTAINER" logs "$API" >"$OUT/logs-api.txt"

{
    printf '# Captured %s by scripts/capture-runtime-fixtures.sh\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '# Project name: %s (deleted at the end of the run)\n' "$PROJECT"
    printf '#\n'
    printf '# container system status --format json  -> system-status.json\n'
    printf '# container system version --format json -> system-version.json\n'
    printf '# container system dns list --format json -> dns-list.json\n'
    printf '# container list --all --format json     -> container-list.json\n'
    printf '# container inspect %s %s -> container-inspect.json\n' "$API" "$WORKER"
    printf '# container volume list --format json    -> volume-list.json\n'
    printf '# container volume inspect %s -> volume-inspect.json\n' "$VOLUME"
    printf '# container network list --format json   -> network-list.json\n'
    printf '# container image list --format json     -> image-list.json\n'
    printf '# container logs %s -> logs-api.txt\n' "$API"
} >"$OUT/capture.txt"

printf 'captured into %s\n' "$OUT"
