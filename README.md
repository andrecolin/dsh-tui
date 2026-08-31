# dsh-tui

A terminal front end for [DSH](https://github.com/deepseek-ai/deepseek-harness) with the
feature surface of `dsh web`. One command, no browser, no bound port.

```sh
dsh-tui
```

Built on DSH; not an official DeepSeek project.

### Running it against a source checkout

```sh
cd bridge && pnpm install --ignore-workspace && pnpm run build && cd ..
cargo build

DSH_TUI_HOST_COMMAND=node \
DSH_TUI_HOST_ARGS="--import tsx/esm apps/cli/src/bin.ts web --no-open --port 0" \
DSH_TUI_HOST_CWD=/path/to/deepseek-harness \
  ./target/debug/dsh-tui --runtime node bridge/lib/runner.js
```

Or, with a release build and the harness at `~/deepseek-harness`, just:

```sh
./run.sh
```

`--screenshot <seconds>` renders one frame as plain text and exits, so the whole stack can be
checked without a terminal; `--view settings|models|plugins|general|workspace|logs` opens a
surface first, `--view rows` dumps the ledger's rows instead of a frame, and
`--size <cols>x<rows>` sets the frame. `--runtime` must come last: it takes every remaining
argument as the child command line.

### Keys

`tab` focus · `^b` sidebar · `^d` details · `^f` search · `^l` logs · `^n` new session ·
`^p` model · `^o` workspace directory · `^s` settings · `^w` workspaces · `^c` quit.

With focus on the **session pane** (`tab` until its border highlights): `↑↓` move through
the sessions, `home`/`end` jump to either end, `enter` opens the highlighted one, and `n`
starts a new one. The status bar shows these while the pane has focus. In the conversation,
`enter` sends.

**Moving the cursor browses; `enter` opens.** They are separate because following on every
arrow rebuilt a transcript stream per keypress. The sidebar header therefore names the
session that is actually open, since the cursor can be anywhere else in the list.

Opening a session **moves the workspace with it** — the status bar, the sidebar header and
the session list all follow, rather than describing a workspace the open session does not
live in. A row that belongs to a different workspace than the current one names it
underneath; rows in the current workspace do not, since repeating it would be noise.

### Workspace

A session is rooted in a workspace, and there is **no default**: the agent's working
directory decides where its edits and deliverables land, so inferring one from wherever the
binary was launched writes another project's files into that directory. The status bar
always names the current workspace, or says `no workspace`.

Two ways in:

- **`^w`** lists the workspaces the host knows. `↑↓` to move, `home`/`end` to jump, `enter`
  to use one, `n` to register a new one. The one currently rooting sessions is marked
  `● in use`.
- **`^o`** browses the filesystem directly. `enter` takes **whatever the cursor is on** —
  the first row stands for the directory you are in, and the rest are its subdirectories,
  so picking a visible subfolder is one key. `→` descends into a folder to look deeper,
  `←` goes up, `.` toggles hidden rows, and `pgup`/`pgdn`/`home`/`end` move through a long
  listing. The list scrolls to keep the cursor in view.

  `←` climbs the host's own crumb chain, so holding it rises a level per press even while
  the listings are still arriving. When a level cannot be reached — the host refuses it, or
  there is no crumb above the current one — the picker says why instead of doing nothing.

  A directory can hold more entries than the host will list; when it does, the picker says
  so. Revealing hidden rows does not recover the cut tail — hidden rows counted toward the
  same bound.

Either route calls `workspace/create`, which adopts the directory as a real workspace on
the host — so the choice survives the run and appears in `^w` next launch. If the harness
does not expose that method the path is still used for the session, and the log says the
persistence was lost.

Pressing `n` with no workspace opens the picker and creates the session once you choose.

The workspace is a **scope**, not just a label: the sidebar names it and lists only its
sessions, and choosing a different one clears a transcript that belongs to a session it
does not contain. Choosing always lands you back in the conversation.

A **new** workspace opens empty and ready — `Nothing here yet` over a live composer. Just
type: the session is created to carry the first prompt, so there is no separate step. A
workspace you **return to** restores its own session and transcript.

A path the host has not told us about is not scoped rather than scoped to nothing, so an
unknown workspace can never blank the sidebar.

### Logs

Every run writes one NDJSON record per event to `~/.local/state/dsh-tui/latest.ndjson`, from
**both** ends of the stack under one correlation key, so a request, the bridge's handling of
it, and the reply read as one exchange. `^l` opens the same records in-app.

```sh
DSH_TUI_LOG=trace ./run.sh                       # error|warn|info|debug|trace|off
DSH_TUI_LOG_PAYLOADS=1 ./run.sh                  # full frame bodies, not just their shapes
tail -f ~/.local/state/dsh-tui/latest.ndjson | jq -c '[.ev, .msg]'
```

Payloads are reduced to their shape by default; `DSH_TUI_LOG_PAYLOADS=1` records prompts,
tool arguments and results in full. Credentials are redacted in **both** modes. See
[docs/LOGGING.md](docs/LOGGING.md) for the record shape, the event vocabulary, and recipes
for reading a run.

## Status

**Every one of the web client's 38 browser rows now has a terminal counterpart.** Four are
complete; the rest have their data model, wire calls, and rendering in place with narrower
gaps listed in the [parity checklist](ARCHITECTURE.md#parity-checklist).

All 38 rows are implemented, four of them complete. What remains is narrower: sixel output
(kitty and iTerm2 draw inline today), richer per-tool cards, and the interaction depth each
row's checklist entry names. The transport, protocol, and frame are working and tested end to end against a
stub bridge; the panes are being filled in. Nothing ships until the
[parity checklist](ARCHITECTURE.md#parity-checklist) is complete — the web client declares
38 browser rows and every one needs a terminal counterpart.

Working today:

- the protocol, in Rust and TypeScript, tested in both directions — including against the
  compiled bridge server, not only a stub
- the bridge, typechecking and building against the real harness client face
- the three-column `AppFrame`, focus ring, and collapsible side columns
- the palette, taken from the web client's own `--dsw-*` tokens
- the session list and the followed conversation, over real remote calls and the
  `session.follow` journal stream
- the journal ledger: packed chunk rows, contiguity, duplicate removal, and gap repair
- the `/` and `@` composer triggers with host-supplied candidates
- the settings form: schemastery envelope read directly, secrets never displayed, writes
  carrying the revision so a stale editor is refused
- the workspace browser over `workspace.follow`, with archived sessions filtered out
- the Models page: the provider directory joined to settings profiles and credential state,
  with per-route readiness
- the plugin inventory: searchable, separating disabled plugins from broken ones
- the General page: theme preference shared with the web UI through the `ui-theme` namespace
- tool cards: calls paired to results through the id inside the result message, per-card
  glyphs, and both failure signals
- the trajectory pane: turns and tool durations timed from the log's own timestamps
- the composer chip strip: goal phase, plan mode, and live background jobs
- the model picker, naming the providers that failed or listed nothing
- the subagent catalog, separating children from the diagnostics explaining absent rows
- skills merged into the `/` menu beside commands
- workflow runs with phased members, and the files a conversation actually produced
- agent and permission presets, including the ones that exist but cannot be chosen
- the workspace directory chooser, navigating by the host's own absolute paths
- image attachments checked against the host's limits, with terminal-graphics detection
- the `ask_user_question` composer takeover, including the plan-review card
- conversation scrollback with backfill paging, and in-transcript search
- the whole-call tree: `run_code` sub-dispatches nested under the program that issued them
- inline images on kitty and iTerm2, with a reasoned placeholder everywhere else
- English and Chinese throughout, chrome and content alike
- the blocking-waterfall modal, including safe delegation for request shapes this build
  cannot render
- runtime supervision: a runtime that dies surfaces as a disconnect rather than a frozen UI

## How it fits together

```
dsh-tui (Rust) ──stdio JSON── dsh-tui-bridge (TS) ──loopback HTTP+WS── dsh web
```

The bridge speaks DSH's Connection wire directly: `POST /api/<endpoint>` for unary calls and
one `/api/remote.mux` WebSocket for streams. The **host** owns validation and serialization,
so no generated code lives in the bridge and none in Rust. See
[ARCHITECTURE.md](ARCHITECTURE.md) and [PROTOCOL.md](PROTOCOL.md).

## Development

[CONTRIBUTING.md](CONTRIBUTING.md) has the full setup, the test split, and the house
style. The short version:

The Rust side develops against a stub bridge, so it needs no harness build:

```sh
cargo test                                          # includes cross-language protocol tests
cargo run -- --runtime node bridge/dev-stub.mjs     # drive the UI against the stub
cargo run -- --runtime node bridge/test-server.mjs  # drive it against the real bridge server
```

Every one of those commands writes a log; `DSH_TUI_LOG=trace` on the front makes a failing
run self-describing, and `docs/LOGGING.md` has the recipes for reading it.

The bridge builds against a local harness checkout, which must itself be built first —
the linked packages resolve their types to `lib/`, which only a build produces:

```sh
cd ../deepseek-harness && pnpm install && pnpm run build && cd -
cd bridge && pnpm install --ignore-workspace && pnpm run typecheck
```

Flags: `--light` / `--dark`, and `--runtime <cmd> [args…]` to replace the spawned runtime.

### Harness dependency

`bridge/package.json` links a local DSH checkout rather than the npm registry:
`@deepseek-ai/dsh-api-session-controller` is unpublished, and the published client-face
packages trail a current checkout by a minor version. Point the `link:` paths at your
checkout and record its commit in `dshTui.harnessCommit`.

The paths resolve to a **sibling of this repository**, so the expected layout is:

```
parent/
├── dsh-tui/
└── deepseek-harness/
```

Those imports are all `import type`, so they vanish at compile time: `bridge/lib/*.js`
pulls in nothing but `node:child_process` and `node:readline`. The checkout is a **build**
dependency, not a runtime one — which is why releases can ship a prebuilt `bridge/lib`
that runs anywhere Node does.

Only the Rust side builds without it. `cargo test --workspace --lib` and the
`bridge_protocol`, `keys` and `render` integration tests run from a clean clone against
`bridge/dev-stub.mjs`; `tests/real_bridge.rs` needs the compiled bridge and is the one
target CI skips.

## Platforms

Linux, macOS and Windows. The terminal layer is `crossterm` and the only Unix-specific
call — the `latest.ndjson` symlink — is already behind `cfg(unix)`.

Binaries are not portable between them, or between glibc versions: build on the target, or
build `--target x86_64-unknown-linux-musl` for a static Linux binary that runs on anything.
A copied install needs four things, not one — the binary, `bridge/lib/`, Node, and
something serving `dsh web`. That last one need not be a source checkout; if `dsh` is on
`PATH`:

```sh
DSH_TUI_HOST_COMMAND=dsh DSH_TUI_HOST_ARGS="web --no-open --port 0" \
  ./dsh-tui --runtime node bridge/lib/runner.js
```

Per-platform detail worth knowing:

- **macOS** — iTerm2 hits the supported inline-image path, so images render rather than
  falling back to a placeholder. Logs land in `~/.local/state/dsh-tui`, which is honest
  rather than idiomatic.
- **Windows** — logs go to `%LOCALAPPDATA%\dsh-tui`, preferred over `HOME` so the location
  is the same whether the binary was started from PowerShell, cmd, or a Git Bash that sets
  `HOME` too. There is no `latest.ndjson`, so read the newest `run-*.ndjson`. Use
  `run.ps1` in place of `run.sh`. No terminal there implements kitty or iTerm2 graphics,
  so images take the placeholder path.

## Support

If dsh-tui is useful to you, you can buy me a coffee at
[ko-fi.com/andrecolin](https://ko-fi.com/andrecolin). Entirely optional — the project is
MIT and stays that way.

## License

MIT — see [LICENSE](LICENSE)
