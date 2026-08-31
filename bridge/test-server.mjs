#!/usr/bin/env node
/**
 * Runs the real `BridgeServer` against a fake backend.
 *
 * The dev stub reimplements the wire; this exercises the bridge's own dispatch, framing,
 * cancellation, and waterfall bookkeeping. What it fakes is only the harness client face.
 */

import { BridgeServer } from './lib/server.js'

const SESSIONS = [
  { id: 's-1', status: 'idle', projections: { asOfSeq: 1, values: { title: 'Real bridge server' } } },
]

const backend = {
  async call(ns, method, args, signal) {
    if (ns === 'session' && method === 'list') return { sessions: SESSIONS }
    if (ns === 'session' && method === 'slow') {
      // Resolves only if not aborted, so a cancel is observable from the client.
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve({ done: true }), 5000)
        signal.addEventListener('abort', () => {
          clearTimeout(timer)
          reject(Object.assign(new Error('aborted'), { code: 'cancelled' }))
        })
      })
    }
    throw Object.assign(new Error(`no ${ns}.${method}`), { code: 'not-found' })
  },

  async *open(stream, args, signal) {
    yield { gen: 1, value: { change: 'replace', records: [] } }
    yield {
      gen: 1,
      value: {
        change: 'append',
        records: [{ type: 'event', event: { type: 'user/message', seq: 1, time: 0, data: { content: 'from the real server' } } }],
      },
    }
    // Stay open until closed, so `close` → `end` is exercised.
    await new Promise((resolve) => signal.addEventListener('abort', resolve))
  },

  namespaces: () => ['session', 'workspace', 'settings'],
  events: () => ['approval/request', 'user-questions/request'],
  host: () => ({ home: process.env.HOME ?? '/' }),
  clientId: () => 'test-server',
}

const server = new BridgeServer(backend, process.stdout)
server.listen(process.stdin)
server.ready()

if (process.env.DSH_TUI_TEST_ASK === '1') {
  // `next` delegating must be distinguishable from an answer on the wire.
  server
    .ask('approval/request', { sessionId: 's-1' }, [{ title: 'Delete build/' }], () => 'DELEGATED')
    .then((outcome) => process.stderr.write(`server: waterfall settled with ${JSON.stringify(outcome)}\n`))
    .catch((error) => process.stderr.write(`server: waterfall rejected: ${error.message}\n`))
}
