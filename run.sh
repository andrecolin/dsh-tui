#!/usr/bin/env bash
# Launch dsh-tui against a DSH source checkout.
#
# The bridge boots `dsh web` itself on an ephemeral loopback port; nothing needs to be
# running beforehand and no port is published.
set -euo pipefail

HARNESS="${DSH_HARNESS:-$HOME/deepseek-harness}"
BIN="${DSH_TUI_BIN:-./target/release/dsh-tui}"

if [[ ! -d "$HARNESS" ]]; then
  echo "harness checkout not found: $HARNESS (set DSH_HARNESS)" >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "binary not found: $BIN — run 'cargo build --release'" >&2
  exit 1
fi
if [[ ! -f bridge/lib/runner.js ]]; then
  echo "bridge not built — run 'cd bridge && pnpm install --ignore-workspace && pnpm run build'" >&2
  exit 1
fi

export DSH_TUI_HOST_COMMAND=node
export DSH_TUI_HOST_ARGS="--import tsx/esm apps/cli/src/bin.ts web --no-open --port 0"
export DSH_TUI_HOST_CWD="$HARNESS"

# `--runtime` consumes every remaining argument as the child command line, so the user's
# own flags must come before it.
exec "$BIN" "$@" --runtime node bridge/lib/runner.js
