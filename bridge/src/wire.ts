/**
 * A direct client of the harness's Connection wire.
 *
 * The original plan was to mount the generated *client face* in Node, the way the browser
 * does. That is not possible: the client packages build to a browser module-loader payload
 * (`window.__ModuleLoader__.load({...})`), not an importable module, so mounting them in
 * Node would mean re-implementing the browser's module loader and bundle transport.
 *
 * Speaking the wire directly turns out to cost far less than it first appeared, because the
 * **host owns validation and serialization**. The Typert descriptors, codecs, and argument
 * checking all run on the host side of `/api`; a client only has to frame the envelope
 * correctly and read the result. No generated code is needed here.
 *
 * Two carriers, exactly as the Gateway documents them:
 * - unary calls: `POST /api/<endpoint>` with a `client-request` envelope;
 * - streams: one `/api/remote.mux` WebSocket multiplexing independently cancellable
 *   logical streams.
 *
 * @module
 */

/**
 * The Gateway-internal endpoints carrying forwarded host events, mirrored from the
 * harness's `packages/api/gateway/src/stream-protocol.ts`.
 *
 * Inlined rather than imported because every harness import in this package is
 * `import type`: `bridge/lib` has to keep running with no checkout present, and these are
 * runtime values. That file is the normative copy — PROTOCOL.md covers the TUI's side of
 * the bridge, not the host's.
 */
const EVENT_STREAM_ENDPOINT = '$events'
const EVENT_RESULT_ENDPOINT = '$events/result'

/** One logical stream's frames, as the mux carries them. */
interface MuxFrame {
  type: 'item' | 'end' | 'error'
  streamId: string
  value?: unknown
  error?: { code?: string; message?: string }
}

/** A failure carrying the wire's own RPC code, so policy rejections stay distinguishable. */
export class WireError extends Error {
  readonly code: string
  readonly details: unknown

  constructor(code: string, message: string, details?: unknown) {
    super(message)
    this.name = 'WireError'
    this.code = code
    this.details = details
  }
}

/** A live connection to one harness host. */
export class HostConnection {
  readonly #base: URL
  readonly #cookie: string
  #socket: WebSocket | undefined
  #socketReady: Promise<WebSocket> | undefined
  #nextStreamId = 1
  readonly #streams = new Map<string, {
    push(frame: MuxFrame): void
    finish(error?: Error): void
  }>()

  private constructor(base: URL, cookie: string) {
    this.#base = base
    this.#cookie = cookie
  }

  /** Request headers carrying the session cookie, or none when the host mints no cookie. */
  get #auth(): Record<string, string> {
    return this.#cookie === '' ? {} : { cookie: this.#cookie }
  }

  /**
   * Exchange a launch token for the session cookie the host's API requires.
   *
   * The host mints the cookie on a root request carrying `?token=`; every later request
   * presents the cookie. This mirrors what a browser does on first load.
   */
  static async open(hostUrl: string): Promise<HostConnection> {
    const url = new URL(hostUrl)
    const token = url.searchParams.get('token')
    const base = new URL('/', url)
    // A host built before browser auth announces a bare URL and mints no cookie. It is a
    // loopback host either way, so the absence of a token is a host property to carry, not
    // a failure: send no cookie and let `/api` answer for itself.
    if (token === null) return new HostConnection(base, '')
    const response = await fetch(new URL(`/?token=${encodeURIComponent(token)}`, base), {
      redirect: 'manual',
    })
    const setCookie = response.headers.get('set-cookie')
    if (setCookie === null) {
      throw new Error(`host did not mint a session cookie (HTTP ${response.status})`)
    }
    return new HostConnection(base, setCookie.split(';')[0] ?? '')
  }

  /** Invoke one unary Remote method. */
  async call(endpoint: string, args: unknown, signal: AbortSignal): Promise<unknown> {
    const rpcId = crypto.randomUUID()
    const response = await fetch(new URL(`api/${endpoint}`, this.#base), {
      method: 'POST',
      headers: { 'content-type': 'application/json', ...this.#auth },
      // The gateway requires exactly one plain-object `args` field in the payload.
      body: JSON.stringify({ type: 'client-request', rpcId, method: endpoint, payload: { args } }),
      signal,
    })
    if (!response.ok) {
      // A host that serves its index but 404s every `/api` endpoint is not missing this
      // one method — it predates the Remote gateway this bridge speaks. Say that, rather
      // than reporting the first call as if it were the only casualty.
      if (response.status === 404) {
        throw new WireError(
          'transport',
          `the harness host does not serve /api/${endpoint}. Its version is likely older `
          + 'than this bridge expects: dsh-tui needs a host serving /api/remote.mux. '
          + 'Build from a harness checkout (see the README) rather than an npm `dsh`.',
        )
      }
      throw new WireError('transport', `HTTP ${response.status} for ${endpoint}`)
    }
    const envelope = (await response.json()) as {
      rpcId?: string
      result?: { ok: boolean; value?: unknown; error?: { code?: string; message?: string; details?: unknown } }
    }
    if (envelope.rpcId !== rpcId) {
      // A mismatched correlation id means the response belongs to another call.
      throw new WireError('transport', `rpcId mismatch for ${endpoint}`)
    }
    const result = envelope.result
    if (result === undefined) throw new WireError('transport', `empty result for ${endpoint}`)
    if (!result.ok) {
      const error = result.error ?? {}
      throw new WireError(error.code ?? 'internal', error.message ?? endpoint, error.details)
    }
    return result.value
  }

  /**
   * Open the Gateway's forwarded-event stream.
   *
   * One more logical stream on the same mux, carrying `ready`, then `emit` for one-way
   * events and `waterfall` for the two that block the agent until a human answers. The
   * opening `ready` frame carries the client id every later result must quote, so the
   * caller has to read it before announcing itself.
   */
  async *events(signal: AbortSignal): AsyncIterable<unknown> {
    for await (const item of this.stream(EVENT_STREAM_ENDPOINT, {}, signal)) {
      yield item.value
    }
  }

  /**
   * Reply to one waterfall the host is blocked on.
   *
   * The unary carrier, not the mux: the host correlates the reply by the `eventId` inside
   * it rather than by the stream it arrived on.
   */
  async eventResult(result: unknown, signal: AbortSignal): Promise<void> {
    await this.call(EVENT_RESULT_ENDPOINT, result, signal)
  }

  /** Open one logical stream over the shared mux socket. */
  async *stream(
    endpoint: string,
    args: unknown,
    signal: AbortSignal,
  ): AsyncIterable<{ gen: number; value: unknown }> {
    const socket = await this.#mux()
    const streamId = `s${this.#nextStreamId++}`
    const queue: MuxFrame[] = []
    let wake: (() => void) | undefined
    let done = false
    let failure: Error | undefined

    this.#streams.set(streamId, {
      push(frame) {
        queue.push(frame)
        wake?.()
      },
      finish(error) {
        done = true
        failure = error
        wake?.()
      },
    })

    const cancel = (): void => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: 'cancel', streamId }))
      }
    }
    signal.addEventListener('abort', cancel, { once: true })
    socket.send(JSON.stringify({ type: 'open', streamId, endpoint, payload: { args } }))

    try {
      while (true) {
        while (queue.length > 0) {
          const frame = queue.shift()!
          if (frame.type === 'item') {
            // One physical carrier per connection, so every item shares its generation.
            yield { gen: 1, value: frame.value }
            continue
          }
          if (frame.type === 'error') {
            throw new WireError(frame.error?.code ?? 'internal', frame.error?.message ?? endpoint)
          }
          return
        }
        if (done) {
          if (failure !== undefined) throw failure
          return
        }
        await new Promise<void>((resolve) => {
          wake = resolve
        })
        wake = undefined
      }
    } finally {
      this.#streams.delete(streamId)
      signal.removeEventListener('abort', cancel)
    }
  }

  /** The shared mux socket, opened once and reused by every logical stream. */
  async #mux(): Promise<WebSocket> {
    if (this.#socket?.readyState === WebSocket.OPEN) return this.#socket
    this.#socketReady ??= new Promise<WebSocket>((resolve, reject) => {
      const url = new URL('api/remote.mux', this.#base)
      url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
      // Node's WebSocket takes headers through this option; the host authenticates the
      // upgrade with the same cookie as the unary calls.
      const socket = new WebSocket(url, { headers: this.#auth } as never)
      socket.addEventListener('message', (event) => {
        let frame: MuxFrame
        try {
          frame = JSON.parse(String((event as MessageEvent).data)) as MuxFrame
        } catch {
          return
        }
        this.#streams.get(frame.streamId)?.push(frame)
      })
      socket.addEventListener('open', () => {
        this.#socket = socket
        resolve(socket)
      }, { once: true })
      const fail = (): void => {
        this.#socketReady = undefined
        const error = new Error('remote.mux carrier closed')
        // Every logical stream shares this socket, so its loss ends all of them.
        for (const stream of this.#streams.values()) stream.finish(error)
        this.#streams.clear()
        reject(error)
      }
      socket.addEventListener('error', fail, { once: true })
      socket.addEventListener('close', fail, { once: true })
    })
    return this.#socketReady
  }

  close(): void {
    this.#socket?.close()
  }
}
