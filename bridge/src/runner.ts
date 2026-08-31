/**
 * The bridge entry point: boot a harness host, connect to it, serve the TUI protocol.
 *
 * `dsh-tui` spawns this; it in turn spawns `dsh web`, which is the shipped composition
 * that already mounts every host capability and serves `/api`. The host binds loopback
 * only, and its launch URL never leaves this process tree.
 *
 * @module
 */

import { spawn, type ChildProcess } from 'node:child_process'
import { createInterface } from 'node:readline'

import * as log from './log.js'
import { EXPECTED_EVENTS } from './protocol.js'
import { BridgeServer, type BridgeBackend } from './server.js'
import { HostConnection, WireError } from './wire.js'

/** Remote namespaces the TUI addresses. Reported in the handshake. */
const NAMESPACES = [
  'session', 'skills', 'fileReferences', 'settings', 'credentials', 'workspace',
  'directoryPicker', 'commands', 'goal', 'agentPresets', 'subagent', 'llm',
  'pluginInventory', 'messageFeedback', 'sessionReference', 'cordisRunner',
]

/** Wait for `dsh web` to announce its loopback URL. */
async function hostUrl(child: ChildProcess): Promise<string> {
  const stdout = child.stdout
  if (stdout === null) throw new Error('harness host produced no stdout')
  const lines = createInterface({ input: stdout })
  // This wait has no timeout by design — a slow host must not be killed — so it is the
  // easiest place in the stack to hang silently. Say so periodically instead: the TUI is
  // showing "Starting the harness runtime…" for as long as this loop runs.
  const seen: string[] = []
  const waiting = setInterval(() => {
    log.warn('host.waiting', 'still waiting for the harness host to announce its URL', {
      seconds: Math.round((Date.now() - started) / 1000),
      lines: seen.length,
      lastLine: seen[seen.length - 1] ?? null,
    })
  }, 5000)
  waiting.unref()
  const started = Date.now()
  try {
    return await scan()
  } finally {
    clearInterval(waiting)
  }

  async function scan(): Promise<string> {
    for await (const line of lines) {
      seen.push(line)
      // The launcher prints one line carrying the URL. A host with browser auth appends
      // its launch token; one without does not, and matching only the token form left
      // this loop waiting forever on a URL that was already on screen.
      const match = /(https?:\/\/\S+)/.exec(line)
      if (match?.[1] !== undefined) {
        lines.close()
        // The URL may carry the launch token, so the log records that one arrived, never
        // the URL itself — and never the token.
        log.info('host.ready', 'the harness host announced its loopback URL', {
          waitedMs: Date.now() - started,
          authenticated: match[1].includes('token='),
        })
        return match[1]
      }
      log.host(line)
    }
    throw new Error('harness host exited before announcing its URL')
  }
}

/**
 * Translate one `session/follow` frame into the TUI's journal shape.
 *
 * The wire delivers `SessionFollowFrame`: an opening `snapshot` carrying the log cut and
 * the window's records, then bare `SessionEventEntry` values as events append. The TUI
 * consumes the change-oriented shape the browser's journal adapter produces, so the
 * translation lives here — the one place that knows both.
 */
function journalItem(frame: unknown): unknown {
  if (typeof frame !== 'object' || frame === null) return frame
  const value = frame as Record<string, unknown>
  if (value['type'] === 'snapshot') {
    return {
      change: 'replace',
      records: value['records'] ?? [],
      // The cut a backwards page must quote, and whether older history exists.
      cursor: value['cursor'],
      hasMore: value['hasMore'] === true,
    }
  }
  // Anything else is one appended record, delivered as the journal's `append`.
  return { change: 'append', records: [frame] }
}

/** Split a `namespace.method` or `namespace/method` name into the wire endpoint. */
function endpointOf(namespace: string, method: string): string {
  return `${namespace}/${method}`
}

async function main(): Promise<void> {
  const command = process.env['DSH_TUI_HOST_COMMAND'] ?? 'dsh'
  const args = (process.env['DSH_TUI_HOST_ARGS'] ?? 'web --no-open --port 0').split(' ')

  // stdout is the TUI protocol; the host's own output must never reach it.
  const cwd = process.env['DSH_TUI_HOST_CWD']
  log.info('host.spawn', `starting ${command}`, { command, args, cwd: cwd ?? null })
  const child = spawn(command, args, {
    stdio: ['ignore', 'pipe', 'inherit'],
    ...cwd === undefined ? {} : { cwd },
  })
  child.on('error', (error) => {
    log.error('host.spawn.failed', error.message, { command })
    process.exit(1)
  })

  const url = await hostUrl(child)
  const connecting = Date.now()
  const connection = await HostConnection.open(url)
  log.info('host.connected', 'opened the wire to the harness host', {
    ms: Date.now() - connecting,
  })

  const backend: BridgeBackend = {
    async call(ns, method, args, signal) {
      return connection.call(endpointOf(ns, method), args ?? {}, signal)
    },
    async *open(stream, args, signal) {
      // The TUI names streams `namespace.method`; the wire wants `namespace/method`.
      const [ns, method] = stream.split('.')
      if (ns === undefined || method === undefined) {
        throw new WireError('invalid-argument', `malformed stream name ${stream}`)
      }
      const frames = connection.stream(endpointOf(ns, method), args ?? {}, signal)
      // Only the session journal needs reshaping; control and workspace frames already
      // match what the TUI reads.
      const adapt = stream === 'session.follow' ? journalItem : (frame: unknown) => frame
      for await (const item of frames) {
        yield { gen: item.gen, value: adapt(item.value) }
      }
    },
    namespaces: () => NAMESPACES,
    events: () => [...EXPECTED_EVENTS],
    host: () => ({}),
    clientId: () => undefined,
  }

  const server = new BridgeServer(backend, process.stdout)
  server.listen(process.stdin)
  server.ready()

  const shutdown = (): void => {
    log.info('bridge.signal', 'received a termination signal')
    server.dispose('bridge shutting down')
    connection.close()
    child.kill()
    process.exit(0)
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)
  child.on('exit', (code) => {
    // The host going away takes the bridge with it, and the TUI reports a disconnect.
    // Without this record the cause looks like the TUI's own transport.
    log.error('host.exit', `the harness host exited with ${code ?? 'no code'}`, {
      code: code ?? null,
    })
    process.exit(code ?? 1)
  })
}

main().catch((error: unknown) => {
  log.error('bridge.fatal', error instanceof Error ? error.message : String(error), {
    stack: error instanceof Error ? error.stack ?? null : null,
  })
  process.exit(1)
})
