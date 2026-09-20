# dsh-tui architecture

A terminal front end for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
with the same feature surface as `dsh web`. One command, no browser, no bound port.

```
$ dsh-tui
└─ dsh-tui (Rust)  ── owns the terminal, renders every pane
     │  stdin/stdout: newline-delimited JSON (the TUI protocol)
     └─ dsh-tui-bridge (Node)  ── spawned as a child
          │  HTTP /api  +  WebSocket /api/remote.mux, over loopback
          └─ dsh web --no-open --port 0  ── the shipped host composition
               dsh-base host plane, typert gateway, forwarded events
```

The host binds loopback on an ephemeral port and its launch URL never leaves this process
tree.

## Why the bridge speaks the wire, not the client face

The original design mounted the generated **client face** in Node, the way the browser does,
so that parity would be structural. That turned out to be impossible, and the reason is
worth recording: the client packages build to a browser module-loader payload —
`lib/client.js` begins `window.__ModuleLoader__.load({...})` — rather than an importable
module. Mounting them in Node would mean re-implementing the browser's module loader,
bundle transport, and boot sequence.

Speaking the Connection wire directly costs far less than it first appeared, because **the
host owns validation and serialization**. Typert's descriptors, argument checking, and
codecs all run on the host side of `/api`. A client only frames the envelope and reads the
result; no generated code is needed in the bridge, and none in Rust.

Two carriers, exactly as the Gateway documents them:

- **Unary**: `POST /api/<namespace>/<method>` with a `client-request` envelope whose payload
  is `{ args }`. Args are keyed by the method's **own parameter names** — `session/list`
  wants `_request`, `commands/list` wants `agentId` — which the gateway reports precisely
  when they are wrong.
- **Streams**: one `/api/remote.mux` WebSocket multiplexing independently cancellable
  logical streams, framed `open`/`cancel` out and `item`/`end`/`error` back.

The host authenticates with a launch token exchanged once for a session cookie, the same
handshake a browser performs on first load.

What the bridge gives up by not mounting the client face is the browser adapter's journal
projection, so it performs that one translation itself: `session/follow` delivers a
`SessionFollowFrame` — an opening `snapshot` carrying the log cut, then bare event entries —
which the bridge maps onto the change-oriented shape the TUI reads. Gap repair, contiguity,
and generation handling live in the Rust ledger rather than in the adapter.

## Process and stream ownership

`dsh-tui` spawns the runtime and owns its lifetime; exit tears down the child. The child's
**stdout carries protocol traffic only** — all runtime logging is forced to stderr, the same
discipline `dsh --profile acp` already requires. Rust reads the child's stderr rather than
letting it corrupt the display.

That stderr carries two things. The bridge's own records arrive as `@dsh-log `-prefixed
NDJSON in the same record shape Rust writes, and are adopted verbatim — level, fields and
exchange key intact. Everything else is the harness host's own output, kept as a record
tagged `host` so it holds its provenance rather than reading as something the bridge said.
The prefix is what separates the two: the host prints whatever it likes, including JSON.

Both ends stamp the same exchange key, and the transport times every exchange — it is the
only place that sees both halves — so one file answers "what happened to this request"
across a language boundary. See [docs/LOGGING.md](docs/LOGGING.md).

## Key routing

`keys::on_key` is a chain of blocks, each of which `return`s, so the **first** match owns
the keystroke. The order is: the blocking waterfall, then every modal
(`questions`/`picker`/`model_picker`/`search`), then the full-column views
(`Logs`, `Settings`, `Workspace`), then the conversation.

**Modals are tested before views on purpose.** The directory picker can be opened from the
workspace list, where `app.view` is still `Workspace`; with the view checked first it kept
consuming keys while the picker sat on screen, so the arrows moved a cursor hidden behind
the modal and `enter` chose the wrong thing.

It lives in the library rather than the binary so `tests/keys.rs` can press real keys. A
binding that never fires is invisible to a test that calls the `App` method directly, which
is how `^w`'s arrows shipped broken.

## Scrolling lists

Every list with a cursor is rendered as a stateful `List` whose `ListState` carries the
selection, so the window follows the cursor. Drawn as a flat `Paragraph` — which the
picker, the workspace list and the sidebar all were — rows past the pane's height are
simply clipped, and the cursor walks off the bottom into entries that can never be seen or
reached. The conversation and the log pane instead use `Viewport`, because they are tails
that grow at the bottom rather than lists with a selection.

Where a surface pages, the page size is **measured from the frame** in `App::measure`
rather than assumed: `picker_page` mirrors the popup geometry in `ui`, since a guessed page
either overshoots on a short terminal or crawls on a tall one.

## The four interaction kinds

The protocol is bidirectional because the harness needs answers, not just attention.

| Kind | Direction | Harness concept |
|---|---|---|
| Unary call | TUI → bridge | `ctx.remote.<ns>.<method>(...)` |
| Journal stream | bridge → TUI | `SessionEventStream` — follow-before-page, `replace`/`prepend`/`append`, gap repair |
| Snapshot stream | bridge → TUI | `SessionControlStream` — full baseline per generation, then deltas |
| Ordinary event | bridge → TUI | 21 one-way forwarded events |
| Waterfall request | **bridge → TUI** | `approval/request`, `user-questions/request` — blocking, needs a result or `next()` |

The waterfalls are why a one-way event feed is not enough: a permission prompt and an
`ask_user_question` suspend the agent until the human answers, and the answer must travel back
through the same event identity. See [PROTOCOL.md](PROTOCOL.md).

## Remote namespaces available to the bridge

Mounted by the client assembly today: `session`, `skills`, `fileReferences`, `settings`,
`credentials`, `workspace`, `directoryPicker`, `commands`, `goal`, `agentPresets`, `subagent`,
`llm`, `pluginInventory`, `messageFeedback`, `sessionReference`, `cordisRunner`.

`session` alone carries: `list` `search` `create` `fork` `rename` `prompt` `cancel`
`updateQueue` `follow` `page` `control` `attachment` `modelCatalog` `selectModel`
`openWorkspacePath` `canOpenWorkspacePath`.

## Forwarded event allowlist

Mirrors `API_REMOTE_FORWARDED_EVENTS` exactly. Widening it requires a matching entry upstream,
so the bridge asserts its own list against the harness's at boot and refuses to start on drift.

```
agent-preset/selected            emit
approval/request                 WATERFALL
api-session/activity             emit
api-session/added                emit
api-session/error                emit
api-session/removed              emit
api-session/status               emit
commands/change                  emit
credentials/reference-updated    emit
cordis/request-run               emit
cordis/request-run-resolved      emit
cordis/dynamic-package           emit
cordis/dynamic-retract           emit
cordis/inspect-query             emit
cordis/inspect-query-resolved    emit
llm/adapters-updated             emit
settings/document-updated        emit
user-questions/request           WATERFALL
```

## Parity checklist

`packages/bundle/web-app/cordis.patch.yml` declares **38 browser rows**. Full parity means every
one has a terminal counterpart. This is the shipping gate.

### Infrastructure (4)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `client-modules` | bridge boot; no Rust counterpart | n/a |
| `client-connection` | `dsh-tui-proto` transport | ▣ done |
| `client-hmr` | dev-only; deliberately dropped | n/a |
| `client-locale` | `locale/` — en + zh dictionaries | ▪ complete |

### Shell and rendering (4)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-renderer` | ratatui frame loop + slot binding | ▣ loop done, slots pending |
| `ui-theme` | `--dsw-*` tokens → truecolor/256 palette; font-size → density | ▣ palette + shared preference |
| `ui-layout` | three-column `AppFrame`, resizable splits, concession behavior | ▣ columns + collapse done |
| `ui-sidebar` | sidebar pane: brand row, New Session, collapse, settings seat | ▣ session rows done |
| `ui-brand-official` | brand occupant, official builds only | ▪ complete |

`ui-slots` and `ui-primitives` are libraries rather than rows: they become the Rust slot
registry and the shared widget set (controls, icons, markdown + math, and the
terminal/read/diff/search/web output cards).

### Conversation (6)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-conversation` | conversation assembly, event/view registries, input state | ▣ composer + caret + triggers |
| `ui-chat` | chat target: nodes, details, actions, scroll state | ▣ rows + scrollback + search |
| `ui-tool` | whole-call tree + per-tool cards | ▪ complete |
| `ui-trajectory` | turn-aware event ledger + timing overview | ▣ turns + timing bars; interaction pending |
| `ui-deliverables` | produced-files row, inline file links | ▣ produced list; links pending |
| `ui-attachment` | image rail, drop target, gallery | ▣ limits + inline kitty/iTerm2 draw |

### Interaction (5)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-approval` | permission modal answering the `approval/request` waterfall | ▣ modal + delegation done |
| `ui-user-questions` | composer takeover + plan-review card | ▪ complete |
| `ui-commands` | `/` command palette | ▣ menu over `commands.list` |
| `ui-input-trigger` | `/` and `@` detection under the caret, candidate menu | ▪ complete |
| `ui-reference` | `@file` / `@session` picker | ▣ files over `fileReferences.list` |

### Session surfaces (6)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-session` | session list + interaction state | ▣ real summaries + title projection |
| `ui-workspace` | workspace browser: rows, add/rename/reorder, search, fork, archive | ▣ rows + archive filtering + select + add |
| `ui-jobs` | background jobs in the session header | ▣ live count; job list pending |
| `ui-goal` | goal strip above the composer | ▣ strip + phases; editing pending |
| `ui-plan` | plan-mode chip | ▪ complete |
| `ui-subagent` | subagent catalog, continuation routing, `@` source | ▣ catalog + diagnostics |

### Model and preset (3)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-model-selection` | `/model` popup + composer model seat | ▣ picker + seat |
| `ui-agent-preset` | preset chip, session-header label, roster management | ▣ roster + brokenness |
| `ui-permission-presets` | `/permission` picker + General-settings row | ▣ chip + switchable options |

### Settings (5)

The generic form reads the serialized schemastery envelope directly — Rust cannot rehydrate
schemastery — and projects `type`, `dict`, `inner`, `list`, and `meta` into editable rows.
Writes are path-addressed ops carrying the view's `revision`, so a stale editor is refused
rather than clobbering a concurrent change. The four section pages below are bespoke
surfaces on top of that base, not just the generic form.

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-settings` | settings scope + schema base | ▣ schema interpreter + generic form |
| `ui-settings-general` | General section, onboarding | ▣ Appearance; onboarding pending |
| `ui-settings-models` | provider rows, API keys, model lists, first-run dialog | ▣ provider rows + readiness |
| `ui-settings-plugins` | feature tabs, host-plane plugin cards | ▣ section tabs; cards pending |
| `ui-settings-plugin-inventory` | read-only searchable plugin catalog | ▪ complete |

### Remaining (5)

| Browser row | Terminal counterpart | Status |
|---|---|---|
| `ui-skill` | `/`-triggered skill source + skill call card | ▣ `/` source; call card pending |
| `ui-message-feedback` | like/dislike + note on finalized messages | ▣ versioned writes; row UI pending |
| `ui-workflow-run` | workflow-run node with nested member disclosure | ▣ runs + phased members |
| `ui-cordis` | dynamic Cordis run/inspect surface | ▣ status model; inspect surface pending |
| `directory-picker-browse` | Miller-column Select Workspace Directory dialog | ▣ browse + crumbs + hidden + select |
| `ui-sidebar-workspace` | the active workspace, and the session list scoped to it | ▪ complete |

Legend: ▢ not started · ▣ in progress · ▪ complete

## Known risks

- **Version skew.** The bridge links a local harness checkout; `dsh-api-session-controller` is
  unpublished and the published client-face packages trail the checkout by a minor version.
  The pinned commit in `bridge/package.json` is the contract until upstream publishes.
- **Developer-preview churn.** Upstream promises compatibility-breaking changes. The boot-time
  event-allowlist assertion turns silent drift into a startup failure.
- **`settings.replace` is forbidden from the form.** The rendered view is redacted, so a
  section rebuilt from it and sent wholesale would delete every secret the wire never
  returned. Only path-addressed `mutate` ops are ever emitted; a test guards it.
- **Secrets never round-trip.** A settings view carries only whether a secret slot is set.
  Writing one goes through the credentials namespace, so the settings form refuses it
  rather than sending a value the document was never meant to hold.
- **A sub-dispatch's arguments are already normalized.** `tool/code-dispatch-start` carries
  a JSON value normalized before dispatch, unlike a root `tool/call`'s raw model string,
  which can be malformed. And a start is logged only when the scheduler enters the tool
  body, so an unpaired start is genuinely running — a call abandoned in the queue logs
  nothing at all.
- **A sub-call whose parent is not loaded is kept, not dropped.** With backwards paging the
  parent may simply be older than the held window, so the row stays at the top level marked
  `parent not loaded` rather than hiding work the agent did.
- **Scrollback is measured from the bottom.** A conversation grows downward, so anchoring
  to the top would shift what a reader is looking at with every appended line. A view held
  above the newest output stops following and says `scrolled` in the status bar, so silence
  is not mistaken for a stalled agent.
- **A backwards page quotes the follow frame's cut.** `throughSeq` is the opening frame's
  inclusive cut, so events appended since cannot slide the page boundaries under the
  reader. The stub rejects any other value.
- **Plan approval is named, never positional.** A `plan-review` intent names the option
  that approves; every other option declines. Reading the first option as approval would
  approve a plan the user rejected — the stub deliberately lists `Reject` first. An intent
  a build does not recognize falls back to the generic option list, because an intent
  changes presentation only and never the answer encoding.
- **A dynamic Cordis `rejected` or `waiting` is not a fault.** Rejected is a human
  declining; waiting is a healthy fiber blocked on services that have not arrived.
  Collapsing either into "failed" sends someone debugging a plugin that is fine.
- **Listed is not selectable.** A broken agent preset stays in the roster — hiding it turns
  "misconfigured" into "gone" — but cannot compose a session. The permission select appends
  `custom` *exactly while it is current*: it is derived from knobs matching no preset, so it
  is shown and never offered as a switch target.
- **A deliverable is a successful mutation.** Only `write`, `edit`, and `str_replace_editor`
  count, their arguments must parse, and an `edit` whose `old_string` equals its
  `new_string` changed nothing — listing its path would claim a file was modified when it
  was not. Failed and still-running calls produce nothing.
- **Workflow runs and members use different failure words**: a member settles
  `completed | failed | cancelled`, a run stops `completed | cancelled | error`.
- **A session summary carries no title.** Identity is `sessionId`, liveness is the boolean
  `running`, and the display title is the `title` **projection**, `null` until the first
  title lands. A blank summary is a provisional session the web renderer labels "New
  Session"; treating a missing title as "Untitled" would misdescribe it.
- **Feedback writes carry `ifVersion`** — the observed version when updating, and `null` to
  require that no item exists when creating. Omitting it is how a concurrent edit gets
  clobbered instead of refused.
- **An inactive subagent has not necessarily finished.** `activity` is sampled at read time
  and encodes no durable outcome: `inactive` means the record is not resident. Labelling it
  "done" would be a claim the catalog never made.
- **A missing projection key means the capability is absent**, never that its value is
  falsey. A harness without plan-mode gets no plan chip at all rather than one reading
  "off", which would offer a control that cannot work. A goal's `activation` is likewise
  process-local and absent from the durable projection, so an active goal a disarmed
  process will not continue says `(disarmed)` instead of implying it will proceed.
- **Tool arguments can be malformed.** `tool/call.arguments` is the raw JSON string exactly
  as the model produced it, unparsed, so a card that assumes valid JSON renders nothing for
  the one call most worth looking at. Failures likewise carry two signals — the model-facing
  `isError` on the result block and the internal `error` identity on the event — and reading
  only one calls half the failures a success.
- **`system` theme has no terminal answer.** A browser reads `prefers-color-scheme`; a
  terminal's closest signal is `COLORFGBG`, which many terminals never export. The
  Appearance row states which signal decided, and says plainly when nothing did.
- **A settled `assistant/message` supersedes the fragments that built it.** Both are in the
  log: the streamed chunk rows and the durable message for the same turn and step.
  Rendering both shows every answer twice.
- **A message's content holds reasoning and prose as separate blocks.** Joining every block's
  text prints the model's thinking as the opening of its answer, so the row projection
  splits them and keeps reasoning in its own dimmed row.
- **Only streamed fragments merge into one paragraph.** `assistant/chunk` and the packed
  chunk rows are pieces of one answer; a complete `assistant/message` is already whole, and
  concatenating two of them runs their text together with no break.
- **Images are written after the frame, over reserved cells.** ratatui paints a grid and
  knows nothing about images, so the layout reserves blank cells and the escape sequence is
  written once the buffer has been flushed — writing first would let the frame paint over
  the image.
- **Each protocol takes what it takes.** kitty's `f=100` is PNG, so a JPEG falls back to a
  placeholder rather than being sent as bytes the terminal rejects; iTerm2 hands data to
  the OS decoder and takes both; sixel needs a raster-and-quantize pipeline this build does
  not have, so it is detected and then deliberately not emitted — malformed sixel would
  corrupt the screen where a placeholder degrades.
- **Terminal images are detected, not assumed.** A browser draws any image inline; a
  terminal can only do so through kitty's graphics protocol, iTerm2 inline images, or
  sixel. Detection is deliberately conservative — claiming a protocol the emulator lacks
  prints escape-sequence garbage across the transcript — and an image that cannot be drawn
  gets a placeholder naming the file, its dimensions, and its size. Inline drawing behind
  the detected capability is still to come.
- **Attachments are admitted before sending.** The host enforces `imageLimits` anyway;
  checking first turns a rejected turn into an immediate message naming the file and the
  limit it hit.
- **Directory navigation never joins path segments.** Every row and crumb carries an
  absolute host path, and `truncated` means the host cut the tail — revealing hidden rows
  does not recover it, because hidden rows counted toward the same bound.
