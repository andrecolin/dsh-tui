# Logging

`dsh-tui` writes one NDJSON record per event to a per-run file, and shows the same records
in an in-app pane. Both halves of the stack — the Rust TUI and the Node bridge — emit the
**same record shape** and stamp the **same exchange key**, so one file shows a request
leaving the TUI, what the bridge did with it, and what came back, in order.

A TUI cannot log to the terminal it owns: stdout carries the protocol and image escapes,
and the alternate screen owns the display. **The file is the log; the pane is a view of
it.**

## Where it goes

| | |
| --- | --- |
| Default directory | `$XDG_STATE_HOME/dsh-tui`, else `~/.local/state/dsh-tui` |
| Per-run file | `run-<epoch-ms>-<pid>.ndjson` |
| Newest run | `latest.ndjson` — a symlink, so `tail -f` survives restarts |
| Retention | the newest 20 runs; older ones are pruned at startup |

```sh
tail -f ~/.local/state/dsh-tui/latest.ndjson | jq -c '[.ev, .msg]'
```

Inside the app, `^l` opens the log pane and its header shows the exact path; `^y` copies
it to the clipboard over OSC 52.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `DSH_TUI_LOG` | `info` | `error` \| `warn` \| `info` \| `debug` \| `trace` \| `off` |
| `DSH_TUI_LOG_PAYLOADS` | unset | `1` records full frame bodies rather than shapes |
| `DSH_TUI_LOG_DIR` | see above | where per-run files land |
| `DSH_TUI_LOG_FILE` | — | an exact file, overriding the directory |
| `DSH_TUI_LOG_MAX_BYTES` | `67108864` | per-run cap; the file stops growing after it, and says so |
| `DSH_TUI_LOG_KEEP` | `20` | how many previous runs survive pruning |

The bridge is spawned as a child and inherits the environment, so setting these once
configures both ends. Nothing needs to be passed through by hand.

```sh
DSH_TUI_LOG=trace DSH_TUI_LOG_PAYLOADS=1 ./run.sh
```

## The record shape

```json
{"ts":1788118507434,"lvl":"debug","src":"tui","ev":"call.out","id":1,"org":"c",
 "msg":"→ session.list","f":{"ns":"session","m":"list","args":{}}}
```

| Field | Meaning |
| --- | --- |
| `ts` | wall clock, epoch ms — absolute, so the file lines up with harness logs |
| `lvl` | `error` \| `warn` \| `info` \| `debug` \| `trace` |
| `src` | `tui`, `bridge`, or `host` (the harness's own unstructured output) |
| `ev` | dotted event name — stable and greppable; the thing to *group by* |
| `id` + `org` | the exchange (see below); absent on records that belong to no exchange |
| `msg` | one-line human summary — what the pane shows |
| `f` | structured detail — what analysis reads |

### The exchange key is `org` + `id`, never `id` alone

Client-originated and bridge-originated ids are **independent number spaces** (see
`PROTOCOL.md`). `c1` is a call the TUI opened; `s1` is a waterfall the bridge opened; they
are unrelated. Filtering on the bare number splices two different exchanges together.

```sh
# one call, both ends
jq -c 'select(.id==1 and .org=="c") | [.src, .ev, .msg]' latest.ndjson
```

In the pane, records render as `#c1` / `#s1`, and typing `c1` into the filter selects
exactly that exchange.

### Events

| Event | Level | Notes |
| --- | --- | --- |
| `app.start` / `app.exit` / `app.panic` | info / error | version, runtime, settings; a panic is recorded before the terminal is restored |
| `transport.spawn` / `transport.exit` | info / error | the runtime child |
| `workspace.chosen` | info | the directory new sessions will be rooted in, and what it replaced |
| `workspace.selected` | info | a workspace taken from the `^w` list, with its id |
| `workspace.followed` | info | the workspace moved because a session from it was opened |
| `session.open` | info | a session opened from the sidebar, with the workspace it belongs to |
| `session.create.retry` | warn | the workspace id was stale; retried by directory |
| `workspace.registered` | info | `workspace/create` settled; `created` distinguishes a new workspace from re-picking a known one |
| `workspace.register.failed` | warn | the host refused the registration; the path is used for this run only |
| `session.create` / `session.create.deferred` / `session.create.cancelled` | info | `cwd` names the root, so a session's directory is answerable after the fact |
| `prompt.sent` / `prompt.accepted` | info / debug | one pair per message: `sent` names the session, `accepted` is the host's receipt. A `sent` with no `accepted` is a message the host never took |
| `prompt.deferred` | info | typed before the workspace had a session; a `session.create` follows |
| `prompt.failed` | error | the message did not go out, with the host's reason. The text is returned to the composer |
| `ready` / `ready.drift` | info / error | the handshake; `drift` is a missing forwarded event |
| `call.out` → `call.ok` \| `call.err` | debug / warn | `ms` on the reply, and the originating method the wire frame does not carry |
| `stream.out` → `stream.item*` → `stream.end` \| `stream.err` | info / trace | `ms` and `items` on the terminal record |
| `stream.generation` | info | a generation change invalidates everything the client holds |
| `ask` → `ask.answer` \| `ask.next` \| `ask.reject` | info | the agent is blocked between the two; `ask.settled` carries how long the human took |
| `event` | debug | a forwarded host event |
| `frame.malformed` | error | a frame that did not parse, with a preview; skipped, never fatal |
| `host.stdout` / `host.stderr` | info | the harness's own output, kept with its provenance |

Durations are recorded on **both** ends. The difference between the bridge's `ms` and the
TUI's `ms` for the same exchange is the stdio round trip — which is what separates a slow
harness from a slow pipe.

## Payloads and redaction

By default a payload is reduced to its **shape**: keys and types survive, strings up to 48
characters are kept verbatim — event kinds, statuses and ids are what make a log readable
— and anything longer becomes `str(N)`. Arrays keep their first 8 items and say how many
were dropped. `bytes` always records the true wire size, so a truncated record still tells
you how big the real frame was.

`DSH_TUI_LOG_PAYLOADS=1` records full bodies instead. That includes **prompts, tool
arguments, tool results and file contents** — everything crossing the wire. It is for a
debugging session, not a default.

**Secrets are dropped in both modes.** Keys matching `apiKey`, `secret`, `password`,
`credential`, `authorization`, `bearer`, `token` (exact — so `inputTokens` counts survive)
and their variants are replaced with `<redacted>` before anything is written. The payload
flag is for analyzing the flow, not for spilling credentials into a file that outlives the
session.

The redaction lists are mirrored in `crates/dsh-tui/src/logging.rs` and
`bridge/src/log.ts`, and each has unit tests. Changing one without the other means the two
ends disagree about what is safe to record.

## The log pane

`^l` from the conversation. It shows what the file received, with the same thresholds — a
record the file filtered out is never shown here, so the pane cannot claim a run the file
does not contain.

| Key | Action |
| --- | --- |
| type | filter — matches messages, events, sources, fields and exchange keys |
| `⌫` | delete a filter character |
| `tab` | narrow the level; wraps at the file's own threshold |
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | scroll; it follows the newest record until you scroll up |
| `^y` | copy the log file's path (OSC 52) |
| `esc` | clear the filter, then leave |

The header shows the file, how many records are hidden by the current filter, and whether
payloads are being captured in full — the three things asked the moment a pane looks
emptier than expected.

## Reading a run

```sh
cd ~/.local/state/dsh-tui

# what happened, in order
jq -r '"\(.ts) \(.src) \(.ev) \(.msg // "")"' latest.ndjson

# slowest calls
jq -r 'select(.ev=="call.ok") | [.f.ms, .msg] | @tsv' latest.ndjson | sort -rn | head

# errors only
jq -c 'select(.lvl=="error" or .lvl=="warn")' latest.ndjson

# how long humans took to answer waterfalls
jq -r 'select(.ev=="ask.settled") | [.f.ms, .f.outcome, .f.event] | @tsv' latest.ndjson

# where the traffic is
jq -r .ev latest.ndjson | sort | uniq -c | sort -rn
```

## Testing it

`cargo test --test bridge_protocol` drives the real transport against `bridge/dev-stub.mjs`
and asserts that an exchange is recorded from both ends under one key, that replies carry
a duration and their originating method, and that the pane's filter cannot reveal a record
the file never took. The redaction rules have unit tests on both sides.
