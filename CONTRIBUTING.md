# Contributing

Thanks for looking. The one thing worth reading before you start is the repository layout —
it is the only real setup trap.

## Layout

`bridge/package.json` links a local DSH checkout rather than the npm registry, and the
`link:` paths resolve to a **sibling of this repository**:

```
parent/
├── dsh-tui/
└── deepseek-harness/
```

`@deepseek-ai/dsh-api-session-controller` is unpublished and the published client-face
packages trail a current checkout by a minor version, so there is no registry-only path.
Clone the harness beside this repo and record its commit in `dshTui.harnessCommit`.

**The harness must be built before the bridge will typecheck.** All three packages resolve
their `types` to `lib/types/*.d.ts`, which is build output — a fresh checkout has none, and
`pnpm install` will still link the packages happily, so the failure surfaces later as
`TS2307: Cannot find module '@deepseek-ai/cordis'` plus a cascade of implicit-`any` errors
from the now-untyped imports. Only the `TS2307`s are real.

```sh
cd ../deepseek-harness
pnpm install
pnpm run build          # or the narrower `pnpm run build:lib`
```

Check out the commit named in `dshTui.harnessCommit` rather than whatever `main` happens to
be. A newer checkout may typecheck, but the pin is what makes a bridge failure attributable;
if you deliberately move to a newer harness, update the pin in the same change.

## What builds without the harness

Most of it. Those bridge imports are all `import type`, so they vanish at compile time and
`bridge/lib/*.js` pulls in nothing but `node:child_process` and `node:readline`. The
checkout is a **build** dependency, not a runtime one.

So from a clean clone, with no harness at all:

```sh
cargo build
cargo test --workspace --lib
cargo test -p dsh-tui --test bridge_protocol --test keys --test render
cargo run -- --runtime node bridge/dev-stub.mjs      # drive the UI against the stub
```

The stub reimplements the wire in plain Node, which is why the protocol tests need no
install step. **This is the loop to develop in** unless you are changing the bridge itself.

The one target that needs more is `tests/real_bridge.rs`: it imports `bridge/lib/server.js`,
which only `pnpm run build` produces and which needs the harness for its types. CI skips
that target for the same reason. If you have the checkout:

```sh
cd bridge && pnpm install --ignore-workspace && pnpm run build && cd ..
cargo test --workspace                               # all 357
```

That needs the harness **built**, not merely installed — the same `pnpm run build:lib` the
Layout section calls for. It earns its keep twice, and the two failures look nothing alike:
without it the bridge cannot typecheck, because the linked packages resolve their types out
of `lib/types/`; and `dsh web` cannot serve, because its client bundle is assembled from
each client package's `lib/`, so the host exits during startup with a module-registry error
naming whichever package it reached first.

`run.sh` checks for the install and the build before it launches anything, and names the
command that fixes each, so prefer it over starting the binary by hand.

## Before opening a PR

```sh
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs the tests above on Linux, macOS and Windows, plus clippy as a denied-warnings gate.
Both must pass.

**Please do not run `cargo fmt`.** The import grouping in this repo is deliberate and
rustfmt disagrees with it; there is no fmt check in CI, and a reformatting diff buries the
change you actually made. Match the surrounding style instead.

That style is comment-heavy in a specific way: comments explain *why* a thing is the way it
is — the constraint, the alternative that was rejected — rather than restating what the code
does. Test names are sentences describing the behaviour under test. Both are worth
imitating.

## Platform notes

The terminal layer is `crossterm`, so all three platforms work. If you touch anything
touching paths or the filesystem, remember Windows sets neither `XDG_STATE_HOME` nor `HOME`
— see `state_dir_from` in `crates/dsh-tui/src/logging.rs`, which takes the platform as an
argument precisely so both resolution orders stay testable from any host. Prefer that shape
over `#[cfg]` when the difference is only which environment variables to read.

## Debugging a failing run

Every run writes one NDJSON record per event, from both ends of the stack under one
correlation key:

```sh
DSH_TUI_LOG=trace cargo run -- --runtime node bridge/dev-stub.mjs
tail -f ~/.local/state/dsh-tui/latest.ndjson | jq -c '[.ev, .msg]'
```

`DSH_TUI_LOG_PAYLOADS=1` records full frame bodies rather than their shapes; credentials are
redacted in both modes. [docs/LOGGING.md](docs/LOGGING.md) has the record shape and recipes
for reading a run.

## Scope

[ARCHITECTURE.md](ARCHITECTURE.md) explains why the bridge speaks the wire rather than the
client face, and carries the
[parity checklist](ARCHITECTURE.md#parity-checklist) — 38 rows the web client declares, each
needing a terminal counterpart. That checklist is the roadmap; picking an incomplete row off
it is the most useful thing you can do.

By contributing you agree your work is licensed under the [MIT License](LICENSE).
