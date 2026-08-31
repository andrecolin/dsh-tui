/**
 * The `dsh-tui-bridge` plugin.
 *
 * Boots a client Cordis environment, mounts the same generated Remote contributions the
 * browser mounts — `@deepseek-ai/dsh-api-remotes/client` over the in-process Connection
 * carrier — and projects that face onto the dsh-tui protocol on stdout.
 *
 * The browser reaches this face over HTTP `/api` and the `/api/remote.mux` WebSocket. In
 * process, `connection.rpc.open` supplies equivalent streams with no socket and no bound
 * port, which is why `dsh-tui` needs neither.
 *
 * @module
 */

import type { Context } from '@deepseek-ai/cordis'
// Type-only import for its declaration merges: this is what puts `ctx.remote` on Context
// and the forwarded-event keys on Cordis `Events`. The runtime values are mounted by the
// api-remotes client plugin in the composition, not imported here.
import type {} from '@deepseek-ai/dsh-api-remotes/client'
import type { ConnectionHandle } from '@deepseek-ai/dsh-client-connection/client'

import * as log from './log.js'
import { EXPECTED_EVENTS, isWaterfall } from './protocol.js'
import { BridgeServer, type BridgeBackend } from './server.js'

export interface BridgeConfig {
  /**
   * Fail boot when the harness forwards a different event set than this bridge expects.
   * Turning it off trades a loud startup failure for surfaces that quietly go dark.
   */
  assertEventParity?: boolean
}

export const name = 'dsh-tui-bridge'

export function apply(ctx: Context, config: BridgeConfig = {}): void {
  const assertParity = config.assertEventParity ?? true

  // stdout is protocol-only. Anything the runtime would otherwise print there would be
  // read as a frame, so logging is redirected before the client face starts producing.
  const out = process.stdout
  redirectConsoleToStderr()

  ctx.inject(['remote', 'connection'], (scope) => {
    // Namespace and method are chosen at runtime by the wire message, which is exactly
    // what a bridge does; the generated face is statically typed for callers that name a
    // method, not for dynamic dispatch, so one documented cast buys the whole surface.
    const remote = scope.remote as unknown as RemoteFace
    // `connection` is a service without a Context declaration merge; the harness's own
    // client plugins reach it exactly this way.
    const connection = scope.get('connection') as ConnectionHandle

    const backend: BridgeBackend = {
      async call(ns, method, args, signal) {
        const namespace = remote[ns]
        if (namespace === undefined) throw rpcError('not-found', `unknown namespace ${ns}`)
        const fn = namespace[method]
        if (typeof fn !== 'function') {
          throw rpcError('not-found', `unknown method ${ns}.${method}`)
        }
        // Generated cancellation-aware methods take a trailing AbortSignal.
        return (await fn.call(namespace, args, signal)) as unknown
      },

      async *open(stream, args, signal) {
        // `$stream` spans physical carrier generations and annotates each item with the
        // generation that produced it; the TUI treats an increment as a new baseline
        // rather than a delta, which is why the generation travels on the wire.
        const [ns, method] = stream.split('.')
        if (ns === undefined || method === undefined) {
          throw rpcError('invalid-argument', `malformed stream name ${stream}`)
        }
        const namespace = remote[ns]
        const fn = namespace?.[method]
        if (typeof fn !== 'function') {
          throw rpcError('not-found', `unknown stream ${stream}`)
        }
        const iterable = fn.call(namespace, args, signal) as AsyncIterable<StreamItem>
        for await (const item of iterable) {
          yield { gen: item.generation ?? 0, value: item.value ?? item }
        }
      },

      namespaces: () => Object.keys(remote).filter((key) => !key.startsWith('$')),
      events: () => [...EXPECTED_EVENTS],
      // Host facts arrive on the current generation's opening frame; the browser reads
      // them the same way to abbreviate displayed paths.
      host: () => {
        const home = connection.generation.getSnapshot()?.host.home
        return home === undefined ? {} : { home }
      },
      clientId: () => undefined,
    }

    const server = new BridgeServer(backend, out)

    // Attach every forwarded-event listener BEFORE announcing readiness, so no event can
    // be lost between mount and the TUI's first read — the same ordering guarantee the
    // Gateway's own `ready` frame gives the browser.
    //
    // The two waterfalls block the agent, and their agent identity arrives as `this`
    // rather than as an argument.
    scope.on('approval/request', function (request, next) {
      return server.ask('approval/request', this, [request], next) as ReturnType<typeof next>
    })
    scope.on('user-questions/request', function (request, next) {
      return server.ask('user-questions/request', this, [request], next) as ReturnType<typeof next>
    })

    for (const event of EXPECTED_EVENTS) {
      if (isWaterfall(event)) continue
      scope.on(event, (...args: unknown[]) => {
        server.emitEvent(event, args)
      })
    }

    if (assertParity) assertEventParity(scope)
    log.info('bridge.mounted', 'the client face is mounted and every listener is attached', {
      namespaces: backend.namespaces().length,
      events: EXPECTED_EVENTS.length,
    })

    server.listen(process.stdin)
    server.ready()

    scope.effect(() => () => server.dispose('bridge unloaded'), 'dsh-tui-bridge: teardown')
  })
}

/**
 * Compare the harness's forwarded-event allowlist against this bridge's expectation.
 *
 * Upstream promises compatibility-breaking changes, and a silently missing event is a
 * terminal pane that renders nothing with no error. Failing at boot names the drift.
 */
function assertEventParity(scope: Context): void {
  const actual = readForwardedEvents(scope)
  if (actual === undefined) return
  const missing = EXPECTED_EVENTS.filter((event) => !actual.includes(event))
  const added = actual.filter((event) => !(EXPECTED_EVENTS as readonly string[]).includes(event))
  if (missing.length === 0 && added.length === 0) return
  const parts = [
    missing.length > 0 ? `no longer forwarded: ${missing.join(', ')}` : '',
    added.length > 0 ? `newly forwarded and unhandled: ${added.join(', ')}` : '',
  ].filter(Boolean)
  throw new Error(`dsh-tui-bridge: forwarded-event drift — ${parts.join('; ')}`)
}

/**
 * Read the live allowlist if the runtime exposes it.
 *
 * INTEGRATION SEAM: the allowlist lives in `API_REMOTE_FORWARDED_EVENTS` on the host face.
 * Reaching it from the client fiber needs either a host-side projection or a direct import
 * of `@deepseek-ai/dsh-api-remotes/types`. Returning `undefined` skips the check rather
 * than failing boot on a runtime that does not publish it.
 */
function readForwardedEvents(scope: Context): string[] | undefined {
  const carrier = scope.remote as { $events?: { keys?: string[] } }
  return carrier.$events?.keys
}

/**
 * Keep stdout clean: every console channel becomes a log record.
 *
 * Anything the runtime prints to stdout would be read as a protocol frame. Routing the
 * channels through the logger rather than raw stderr means a library's `console.warn`
 * arrives in the same file, at the right level, alongside the frames around it.
 */
function redirectConsoleToStderr(): void {
  const route = (level: log.Level) => (...args: unknown[]): void => {
    log.log(level, 'console', args.map(String).join(' '))
  }
  console.log = route('info')
  console.info = route('info')
  console.warn = route('warn')
  console.debug = route('debug')
  console.error = route('error')
}

function rpcError(code: string, message: string): Error & { code: string } {
  return Object.assign(new Error(message), { code })
}

/** The generated client face, viewed as the dynamic namespace map a bridge dispatches on. */
type RemoteFace = Record<string, Record<string, unknown> | undefined>

interface StreamItem {
  generation?: number
  value?: unknown
}
