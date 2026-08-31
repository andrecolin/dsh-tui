# The dsh-tui protocol

Newline-delimited JSON, one message per line, over the runtime child's stdin/stdout.
Rust is the **client**; the bridge is the **server**. Both sides may originate messages.

Protocol version: **1**. The bridge refuses to serve a client that announces a different
major version.

## Framing

One JSON object per `\n`-terminated line, UTF-8, no embedded newlines. Malformed lines are
logged to stderr and skipped rather than fatal — a corrupt frame must not desynchronize the
stream. Every message has a `t` discriminator. `id` is a client-allocated `u64` on
client-originated exchanges, and a bridge-allocated `u64` on bridge-originated ones; the two
id spaces are independent.

The child's stdout carries protocol traffic **only**. Runtime logging goes to stderr, which
Rust reads: the bridge's own records are `@dsh-log `-prefixed NDJSON and are adopted as
records, and anything else is the harness host's output.

Because the two id spaces are independent, a bare `id` does **not** identify an exchange.
Every log record carrying an `id` also carries `org` — `c` for client-originated, `s` for
bridge-originated — and it is the pair that correlates. See [docs/LOGGING.md](docs/LOGGING.md).

## Handshake

The bridge sends `ready` once its client face has mounted and its forwarded-event listeners
are attached — the same guarantee the Gateway's own `ready` frame gives the browser, so no
event can be missed between mount and first read.

```json
{"t":"ready","protocol":1,"clientId":"…","host":{"home":"/home/acp"},
 "namespaces":["session","workspace","settings","…"],
 "events":["approval/request","api-session/status","…"]}
```

Rust asserts that every namespace and event it depends on is present, and exits with a
diagnostic naming the missing ones rather than failing later at first use.

## Unary calls

```json
→ {"t":"call","id":7,"ns":"session","m":"list","args":{}}
← {"t":"ok","id":7,"v":{"sessions":[…]}}
← {"t":"err","id":7,"code":"internal","message":"…","data":null}
→ {"t":"cancel","id":7}
```

`cancel` maps onto the `AbortSignal` a cancellation-aware Remote method declares. Cancelling
an unknown or settled id is a no-op, not an error. Error `code` is the wire RPC code preserved
by the Gateway; lookup-policy failures keep their original code, so an ownership fence or a
cold-resume rejection stays distinguishable from a generic `internal`.

## Streams

`session.follow` is a journal stream, `session.control` a snapshot stream. Both are opened by
the client and carry a **generation**: on carrier replacement the bridge re-opens and the
first item of the new generation is a fresh baseline.

```json
→ {"t":"open","id":12,"stream":"session.follow","args":{"sessionId":"…"}}
← {"t":"item","id":12,"gen":1,"v":{"change":"replace","records":[…]}}
← {"t":"item","id":12,"gen":1,"v":{"change":"append","records":[…]}}
← {"t":"end","id":12,"reason":"disposed"}
← {"t":"streamErr","id":12,"code":"…","message":"…"}
→ {"t":"close","id":12}
```

A journal item's `change` is one of `replace`, `prepend`, `append`, matching
`RemoteJournalStream`. Records are `SessionHistoryRecord`: `{type:"event"|"chunks", event}`
where the inner value carries `type`, `seq`, `time`, `data`. A `chunks` record packs
consecutive same-block `assistant/chunk` deltas — its `seq` and `time` are its first member's,
and it covers `[seq, seq + memberCount - 1]`. Rust must expand packed rows for display while
preserving the range for gap detection.

**Generation changes are not deltas.** When `gen` increments, the client discards prior
control state for that stream and adopts the new baseline. This is the documented reason
`SessionControlStream` opens with a complete process-local baseline every generation: queue
and job state are transient, not durable events.

## Ordinary events

One-way, no reply, registration-order delivery. Not replayed after reconnect.

```json
← {"t":"event","event":"api-session/status","args":[{"sessionId":"…","status":"running"}]}
```

## Waterfall requests

`approval/request` and `user-questions/request` are Agent-scoped waterfalls: the harness is
**blocked** on the answer. The bridge originates these, and exactly one of three replies must
follow, or the agent hangs.

```json
← {"t":"ask","id":3,"event":"approval/request","agent":{"sessionId":"…"},"args":[{…}]}

→ {"t":"answer","id":3,"v":{"decision":"allow"}}   // resolve with a result
→ {"t":"next","id":3}                              // delegate to the next host listener
→ {"t":"reject","id":3,"message":"…"}              // fail the waterfall
```

`next` matters: declining to answer is not the same as denying. A TUI that cannot render a
particular request — an unknown approval shape after an upstream change — must send `next` so
the host's own fallback listener handles it, rather than inventing a decision on the user's
behalf.

Answers must be lossless JSON; the bridge rejects a non-JSON-safe result before it reaches the
host, and Rust holds the pending id across a redraw or a pane change so an answer is never
dropped because the user navigated away.

## Local submission echo

`session.beginSubmission` inserts a pending submission synchronously so the composer can show
a message on the submit keystroke's own frame, before any durable event exists. The bridge
exposes it as a call returning a `requestId`, and echoes retire when the matching durable event
or queue occurrence arrives.

```json
→ {"t":"call","id":21,"ns":"session","m":"beginSubmission","args":{"sessionId":"…"}}
← {"t":"ok","id":21,"v":{"requestId":"…"}}
```

Echoes are client memory only. On reconnect Rust rebuilds the conversation from durable events
alone and drops every unretired echo — the same rule the browser follows on reload.

## Shutdown

```json
→ {"t":"shutdown"}
← {"t":"bye"}
```

Rust waits briefly for `bye`, then terminates the child. On an unclean exit the child is killed
and the terminal is restored regardless — a panic in the renderer must never leave the terminal
in raw mode.
