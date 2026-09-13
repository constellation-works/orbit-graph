#!/usr/bin/env bash
# Shared configuration for the Milestone 5 change-explorer evaluation.
#
# Every script in this directory sources this file. Nothing here writes into a
# sibling checkout: corpora are cloned into $SCRATCH/repos and every explorer
# invocation is given an explicit --cache-dir under $SCRATCH/cache.

set -euo pipefail

# Repository holding the explorer under evaluation (this checkout).
REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd -P)}"
# Scratch root. Must not be inside REPO_ROOT: the evaluation must leave no
# untracked artifacts in the worktree.
SCRATCH="${SCRATCH:-/tmp/orb-12393}"
# Where the sibling checkouts are cloned from.
CODEBASES="${CODEBASES:-$HOME/workspace/constellation/codebases}"

BIN_DIR="${BIN_DIR:-$REPO_ROOT/target/release}"
EXPLORER="$BIN_DIR/orbit-graph-explorer"
GRAPH="$BIN_DIR/orbit-graph"

REPOS="$SCRATCH/repos"
CACHE="$SCRATCH/cache"
OUT="$SCRATCH/out"
LOGS="$SCRATCH/logs"

# study id | repo | base ref | head ref | short label
STUDIES=(
  "1|orbit-graph|8ff25d082567cf8d22b25e393160565f3490f9f1|9e5c15986b14c51665145b1df84de7623c70dcd2|resolver"
  "2|orbit|1ca6416e0ba2c6a713e42561b7b35a72b045aa25|5a5b45fec9a83b765b485043398d3abadaa468de|workspace-teardown"
  "3|orbit|ab6135e11aeba511c7dbb2c20e766c78d166c208|156dc93d940ee60c4b1c7cd84db620e128977ff2|complexity-type"
  "4|observatory|0d05e34ef4467aa597c092ffba2b6e6b34506f09|ae145d1c361a82ba94b501a467678d8a6b62fad9|fput-mode-labels"
  "5|orrery|148b668391f575466282eaa81aab0dc943846a87|82c24be9a49dbf9d775db9dc980c1de712d8bb8d|research-records"
)

study_field() { # study_field <study-line> <1-based index>
  printf '%s' "$1" | cut -d'|' -f"$2"
}

require_binaries() {
  for bin in "$EXPLORER" "$GRAPH"; do
    if [ ! -x "$bin" ]; then
      echo "missing $bin; run: cargo build --workspace --locked --release" >&2
      exit 1
    fi
  done
}

# Start `orbit-graph-explorer serve` for one study and export SERVICE_ORIGIN,
# SERVICE_TOKEN and SERVICE_PID. Callers must call stop_service.
start_service() { # start_service <repo> <base> <head> <cache-subdir> [extra args...]
  local repo="$1" base="$2" head="$3" cachedir="$4"; shift 4
  local banner="$LOGS/serve-$cachedir.log"
  mkdir -p "$LOGS"
  : > "$banner"
  "$EXPLORER" serve --repo "$REPOS/$repo" --base "$base" --head "$head" \
    --cache-dir "$CACHE/$cachedir" "$@" 2> "$banner" &
  SERVICE_PID=$!
  for _ in $(seq 1 600); do
    if grep -q '^Authorization: Bearer ' "$banner" 2>/dev/null; then break; fi
    if ! kill -0 "$SERVICE_PID" 2>/dev/null; then
      echo "explorer serve exited early; see $banner" >&2
      cat "$banner" >&2
      exit 1
    fi
    sleep 0.1
  done
  SERVICE_ORIGIN=$(sed -n 's/^orbit-graph-explorer listening on //p' "$banner" | head -1)
  SERVICE_TOKEN=$(sed -n 's/^Authorization: Bearer //p' "$banner" | head -1)
  export SERVICE_ORIGIN SERVICE_TOKEN SERVICE_PID
}

stop_service() {
  if [ -n "${SERVICE_PID:-}" ] && kill -0 "$SERVICE_PID" 2>/dev/null; then
    kill "$SERVICE_PID" 2>/dev/null || true
    wait "$SERVICE_PID" 2>/dev/null || true
  fi
  unset SERVICE_PID
}

api() { # api <path-and-query>
  curl -sS -H "Authorization: Bearer $SERVICE_TOKEN" "$SERVICE_ORIGIN$1"
}

# Block until both sides report ready, or fail after the timeout.
wait_ready() { # wait_ready [seconds]
  local limit="${1:-900}" waited=0
  while [ "$waited" -lt "$limit" ]; do
    local state
    state=$(api /api/health | jq -r '.indexing_status // "unknown"')
    case "$state" in
      ready) return 0 ;;
      failed|cancelled) echo "indexing $state" >&2; return 1 ;;
    esac
    sleep 1
    waited=$((waited + 1))
  done
  echo "indexing did not become ready within ${limit}s" >&2
  return 1
}
