/**
 * The protocol server: framing, dispatch, and the bookkeeping that keeps a blocked agent
 * from hanging. Depends only on `./protocol.ts`, so it is testable without a harness.
 * @module
 */

import type { Readable, Writable } from 'node:stream'

import * as log from './log.js'
import {
  isWaterfall,
  PROTOCOL_VERSION,
  type ClientMsg,
  type ExchangeId,
  type ServerMsg,
} from './protocol.js'

/** What the plugin half supplies: the live client face, reduced to four operations. */
export interface BridgeBackend {
  /** Invoke `ctx.remote.<ns>.<method>(args)`. */
  call(ns: string, method: string, args: unknown, signal: AbortSignal): Promise<unknown>
  /** Open a journal or snapshot stream. Each yielded item carries its generation. */
  open(
    stream: string,
    args: unknown,
    signal: AbortSignal,
  ): AsyncIterable<{ gen: number; value: unknown }>
  /** Remote namespaces mounted on the client face. */
  namespaces(): string[]
  /** Forwarded events actually being listened for. */
  events(): string[]
  /** Host facts from the opening `ready` frame. */
  host(): { home?: string }
  /** Client identity from the opening `ready` frame. */
  clientId(): string | undefined
}

/** One waterfall the harness is blocked on, awaiting the TUI's reply. */
interface PendingAsk {
  resolve(value: unknown): void
  /** Delegate to the next host listener. */
  delegate(): void
  reject(reason: Error): void
}

export class BridgeServer {
  readonly #backend: BridgeBackend
  readonly #out: Writable
  readonly #calls = new Map<ExchangeId, AbortController>()
  readonly #streams = new Map<ExchangeId, AbortController>()
  readonly #asks = new Map<ExchangeId, PendingAsk>()
  /** When each exchange started here, so a reply can be reported with a duration. */
  readonly #started = new Map<ExchangeId, number>()
  #nextAskId = 1
  #closed = false

  constructor(backend: BridgeBackend, out: Writable) {
    this.#backend = backend
    this.#out = out
  }

  /** Announce readiness. Call only after every forwarded-event listener is attached. */
  ready(): void {
    const namespaces = this.#backend.namespaces()
    const events = this.#backend.events()
    log.info(
      'bridge.ready',
      `mounted ${namespaces.length} namespaces, ${events.length} forwarded events`,
      { protocol: PROTOCOL_VERSION, namespaces, events, payloads: log.payloads() },
    )
    this.#send({
      t: 'ready',
      protocol: PROTOCOL_VERSION,
      ...(this.#backend.clientId() === undefined ? {} : { clientId: this.#backend.clientId()! }),
      host: this.#backend.host(),
      namespaces: this.#backend.namespaces(),
      events: this.#backend.events(),
    })
  }

  /** Read client messages off `input` until it ends. */
  listen(input: Readable): void {
    let buffer = ''
    input.setEncoding('utf8')
    input.on('data', (chunk: string) => {
      buffer += chunk
      let newline = buffer.indexOf('\n')
      while (newline >= 0) {
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        this.#onLine(line)
        newline = buffer.indexOf('\n')
      }
    })
  }

  /**
   * Forward a one-way host event.
   *
   * Non-JSON payloads are dropped rather than sent: the host rejects them upstream too,
   * and a half-serialized event is worse than a missing one.
   */
  emitEvent(event: string, args: unknown[]): void {
    if (!isJsonSafe(args)) {
      // A dropped event is a pane that silently stops updating, so it is a warning
      // rather than a debug note.
      log.warn('event.dropped', `dropped a non-JSON payload for ${event}`, { event })
      return
    }
    log.debug('event.out', `→ ${event}`, { event, args: log.describe(args) })
    this.#send({ t: 'event', event, args })
  }

  /**
   * Forward a waterfall and wait for the human.
   *
   * Resolves with the TUI's answer, or calls `next()` when the TUI delegates. The promise
   * never settles on its own: a waterfall with no reply is an agent that hangs, so the
   * caller owns any timeout policy.
   */
  ask(event: string, agent: unknown, args: unknown[], next: () => unknown): Promise<unknown> {
    if (!isWaterfall(event)) {
      return Promise.reject(new Error(`${event} is not a waterfall event`))
    }
    const id = this.#nextAskId++
    const started = Date.now()
    // The agent is blocked from this line until the TUI replies. The paired
    // `ask.settled` carries the same id and the time the human took.
    log.info('ask.out', `→ ask ${event} (agent blocked)`, {
      event,
      agent: log.describe(agent),
      args: log.describe(args),
    }, log.server(id))
    const settle = (outcome: string): void => {
      log.info('ask.settled', `waterfall ${event} ${outcome}`, {
        event,
        outcome,
        ms: Date.now() - started,
      }, log.server(id))
    }
    return new Promise<unknown>((resolve, reject) => {
      this.#asks.set(id, {
        resolve: (value) => {
          settle('answered')
          resolve(value)
        },
        delegate: () => {
          settle('delegated')
          resolve(next())
        },
        reject: (reason) => {
          settle('rejected')
          reject(reason)
        },
      })
      this.#send({ t: 'ask', id, event, agent, args })
    })
  }

  /** Fail every pending exchange. Used when the client goes away mid-flight. */
  dispose(reason: string): void {
    log.info('bridge.dispose', reason, {
      calls: this.#calls.size,
      streams: this.#streams.size,
      asks: this.#asks.size,
    })
    this.#closed = true
    for (const controller of this.#calls.values()) controller.abort()
    for (const controller of this.#streams.values()) controller.abort()
    this.#calls.clear()
    this.#streams.clear()
    // A blocked agent must not wait on a client that is gone.
    for (const ask of this.#asks.values()) ask.reject(new Error(reason))
    this.#asks.clear()
  }

  #onLine(line: string): void {
    const trimmed = line.trim()
    if (trimmed === '') return
    let msg: ClientMsg
    try {
      msg = JSON.parse(trimmed) as ClientMsg
    } catch (error) {
      // Skip, never throw: one corrupt frame must not desynchronize the stream.
      log.error('frame.malformed', 'skipped a client frame that did not parse', {
        error: String(error),
        bytes: trimmed.length,
        preview: trimmed.slice(0, 200),
      })
      return
    }
    // A reply to a waterfall carries the *bridge's* id, not one from the client's
    // space, so it must not be filed under `c`.
    const replies = msg.t === 'answer' || msg.t === 'next' || msg.t === 'reject'
    log.debug('frame.in', `← ${msg.t}`, { t: msg.t },
      'id' in msg ? (replies ? log.server(msg.id) : log.client(msg.id)) : undefined)
    void this.#dispatch(msg)
  }

  async #dispatch(msg: ClientMsg): Promise<void> {
    switch (msg.t) {
      case 'call':
        return this.#onCall(msg.id, msg.ns, msg.m, msg.args)
      case 'cancel': {
        // Cancelling an unknown or settled id is a no-op, not an error — but which of
        // the two it was is the whole question when a cancel appears not to work.
        const live = this.#calls.get(msg.id)
        live?.abort()
        log.debug('call.cancel', live === undefined ? 'cancel for a settled call' : 'cancelled', {
          live: live !== undefined,
          ms: Date.now() - (this.#started.get(msg.id) ?? Date.now()),
        }, log.client(msg.id))
        return
      }
      case 'open':
        return this.#onOpen(msg.id, msg.stream, msg.args)
      case 'close': {
        const live = this.#streams.get(msg.id)
        live?.abort()
        log.debug('stream.close', live === undefined ? 'close for an ended stream' : 'closing', {
          live: live !== undefined,
        }, log.client(msg.id))
        return
      }
      case 'answer': {
        const ask = this.#takeAsk(msg.id)
        ask?.resolve(msg.v)
        return
      }
      case 'next': {
        const ask = this.#takeAsk(msg.id)
        ask?.delegate()
        return
      }
      case 'reject': {
        const ask = this.#takeAsk(msg.id)
        ask?.reject(new Error(msg.message))
        return
      }
      case 'shutdown': {
        this.#send({ t: 'bye' })
        this.dispose('shutting down')
        return
      }
    }
  }

  #takeAsk(id: ExchangeId): PendingAsk | undefined {
    const ask = this.#asks.get(id)
    if (ask !== undefined) this.#asks.delete(id)
    return ask
  }

  async #onCall(id: ExchangeId, ns: string, method: string, args: unknown): Promise<void> {
    const controller = new AbortController()
    this.#calls.set(id, controller)
    const started = Date.now()
    this.#started.set(id, started)
    log.debug('call.in', `← call ${ns}.${method}`, { ns, m: method, args: log.describe(args) }, log.client(id))
    try {
      const value = await this.#backend.call(ns, method, args, controller.signal)
      log.debug('call.ok', `→ ok ${ns}.${method}`, {
        ns, m: method, ms: Date.now() - started, v: log.describe(value),
      }, log.client(id))
      this.#send({ t: 'ok', id, v: value ?? null })
    } catch (error) {
      const described = describe(error)
      // Always recorded: this is the line someone opens the log to find.
      log.warn('call.err', `→ err ${ns}.${method} [${described.code}]: ${described.message}`, {
        ns, m: method, ms: Date.now() - started, ...described,
      }, log.client(id))
      this.#send({ t: 'err', id, ...described })
    } finally {
      this.#calls.delete(id)
      this.#started.delete(id)
    }
  }

  async #onOpen(id: ExchangeId, stream: string, args: unknown): Promise<void> {
    const controller = new AbortController()
    this.#streams.set(id, controller)
    const started = Date.now()
    this.#started.set(id, started)
    let items = 0
    let generation = -1
    log.info('stream.in', `← open ${stream}`, { stream, args: log.describe(args) }, log.client(id))
    try {
      for await (const item of this.#backend.open(stream, args, controller.signal)) {
        if (controller.signal.aborted) break
        items += 1
        // A generation change invalidates everything the client holds, so it is worth a
        // record of its own rather than one field on a trace-level item.
        if (item.gen !== generation) {
          log.info('stream.generation', `${stream} moved to generation ${item.gen}`, {
            stream, gen: item.gen, after: items - 1,
          }, log.client(id))
          generation = item.gen
        }
        log.trace('stream.item', `→ item #${items}`, {
          stream, gen: item.gen, n: items, v: log.describe(item.value),
        }, log.client(id))
        this.#send({ t: 'item', id, gen: item.gen, v: item.value })
      }
      const reason = controller.signal.aborted ? 'disposed' : 'complete'
      log.info('stream.end', `→ end ${stream} (${reason})`, {
        stream, reason, items, ms: Date.now() - started,
      }, log.client(id))
      this.#send({ t: 'end', id, reason })
    } catch (error) {
      const { code, message } = describe(error)
      log.error('stream.err', `→ ${stream} failed [${code}]: ${message}`, {
        stream, code, message, items, ms: Date.now() - started,
      }, log.client(id))
      this.#send({ t: 'streamErr', id, code, message })
    } finally {
      this.#streams.delete(id)
      this.#started.delete(id)
    }
  }

  #send(msg: ServerMsg): void {
    if (this.#closed && msg.t !== 'bye') return
    this.#out.write(`${JSON.stringify(msg)}\n`)
  }
}

/**
 * Map a thrown value onto the wire error shape, preserving an RPC `code` when the Gateway
 * supplied one so a policy rejection stays distinguishable from a generic failure.
 */
function describe(error: unknown): { code: string; message: string; data?: unknown } {
  if (typeof error === 'object' && error !== null && 'code' in error) {
    const carried = error as { code?: unknown; message?: unknown; data?: unknown }
    return {
      code: typeof carried.code === 'string' ? carried.code : 'internal',
      message: typeof carried.message === 'string' ? carried.message : String(error),
      ...(carried.data === undefined ? {} : { data: carried.data }),
    }
  }
  return { code: 'internal', message: error instanceof Error ? error.message : String(error) }
}

/** Whether a value survives a JSON round trip losslessly. */
function isJsonSafe(value: unknown): boolean {
  try {
    JSON.parse(JSON.stringify(value))
    return true
  } catch {
    return false
  }
}
