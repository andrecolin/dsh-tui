#!/usr/bin/env bash
# Launch dsh-tui against a DSH source checkout.
#
# The bridge boots `dsh web` itself on an ephemeral loopback port; nothing needs to be
# running beforehand and no port is published.
set -euo pipefail

HARNESS="${DSH_HARNESS:-$HOME/deepseek-harness}"

# Prefer a release build, fall back to the debug one. Requiring release meant a first-time
# clone that had only run `cargo build` was told the binary did not exist.
if [[ -n "${DSH_TUI_BIN:-}" ]]; then
  BIN="$DSH_TUI_BIN"
elif [[ -x ./target/release/dsh-tui ]]; then
  BIN=./target/release/dsh-tui
else
  BIN=./target/debug/dsh-tui
fi

die() { echo "run.sh: $1" >&2; exit 1; }

if [[ ! -d "$HARNESS" ]]; then
  die "harness checkout not found: $HARNESS
  Clone it, or point DSH_HARNESS at your copy:
    git clone https://github.com/deepseek-ai/deepseek-harness ~/deepseek-harness"
fi
if [[ ! -x "$BIN" ]]; then
  die "dsh-tui binary not found: $BIN — run 'cargo build --release'"
fi
if [[ ! -f bridge/lib/runner.js ]]; then
  die "bridge not built — run 'cd bridge && pnpm install --ignore-workspace && pnpm run build'"
fi

# The next two are the checks this script used to lack, and both failed far downstream:
# without them the TUI came up and reported a bare disconnect, with the real cause only in
# the log. Name them here, where the fix is one command.
if [[ ! -x "$HARNESS/node_modules/.bin/tsx" ]]; then
  die "harness dependencies not installed — run 'pnpm install' in $HARNESS"
fi
if [[ ! -f "$HARNESS/packages/client/ui-user-questions/lib/client.js" ]]; then
  die "harness client libraries not built — run 'pnpm run build:lib' in $HARNESS
  ('dsh web' serves a client bundle; without it the host exits during startup.)"
fi

export DSH_TUI_HOST_COMMAND=node
export DSH_TUI_HOST_ARGS="--import tsx/esm apps/cli/src/bin.ts web --no-open --port 0"
export DSH_TUI_HOST_CWD="$HARNESS"

# `--runtime` consumes every remaining argument as the child command line, so the user's
# own flags must come before it.
exec "$BIN" "$@" --runtime node bridge/lib/runner.js
