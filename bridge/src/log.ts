/**
 * Structured logging, bridge side.
 *
 * Emits the same record shape `crates/dsh-tui/src/logging.rs` writes, prefixed so the TUI
 * can tell our records apart from the harness host's own output — both share stderr, and
 * the host prints whatever it likes, including JSON.
 *
 * stdout is the protocol and cannot carry a byte of logging; stderr is the only channel
 * out of this process, and the TUI is the only reader. A record written here is written
 * to the TUI's log file by the TUI, with the exchange id intact, so one file holds both
 * halves of every exchange in the order they happened.
 *
 * Configuration mirrors the Rust side, and the TUI passes its own settings down when it
 * spawns the bridge:
 *
 * - `DSH_TUI_LOG` — `error` | `warn` | `info` | `debug` | `trace` | `off` (default `info`)
 * - `DSH_TUI_LOG_PAYLOADS` — `1` to record full frame bodies rather than shapes
 *
 * @module
 */

/** The prefix that marks a line as one of ours. Mirrors `logging::BRIDGE_PREFIX`. */
const PREFIX = '@dsh-log '

export type Level = 'error' | 'warn' | 'info' | 'debug' | 'trace'

/**
 * Which side opened an exchange.
 *
 * Client- and bridge-originated ids are independent number spaces, so a bare id does not
 * identify an exchange: `c1` is a call the TUI opened, `s1` a waterfall this bridge
 * opened, and they are unrelated.
 */
export type Origin = 'c' | 's'

/** One exchange: the originating side and its id. */
export interface Exchange {
  origin: Origin
  id: number
}

/** A client-originated exchange — a call or a stream. */
export const client = (id: number): Exchange => ({ origin: 'c', id })

/** A bridge-originated exchange — a waterfall blocking the agent. */
export const server = (id: number): Exchange => ({ origin: 's', id })

const ORDER: Record<Level, number> = { error: 0, warn: 1, info: 2, debug: 3, trace: 4 }

function configuredLevel(): number {
  const raw = (process.env['DSH_TUI_LOG'] ?? 'info').trim().toLowerCase()
  if (raw === 'off') return -1
  if (raw === 'err') return ORDER.error
  if (raw === 'warning') return ORDER.warn
  if (raw === 'dbg') return ORDER.debug
  if (raw === 'trc') return ORDER.trace
  return ORDER[raw as Level] ?? ORDER.info
}

const THRESHOLD = configuredLevel()
const PAYLOADS = ['1', 'true', 'yes', 'on'].includes(
  (process.env['DSH_TUI_LOG_PAYLOADS'] ?? '').trim().toLowerCase(),
)

/** Whether a record at this level would be kept. Guard expensive field building with it. */
export function enabled(level: Level): boolean {
  return THRESHOLD >= 0 && ORDER[level] <= THRESHOLD
}

/** Whether full frame bodies are being recorded. */
export function payloads(): boolean {
  return PAYLOADS
}

export interface Fields {
  [key: string]: unknown
}

/**
 * Write one record.
 *
 * `exchange` is the correlation key: it is what joins this record to the TUI's own record
 * for the same call, stream or waterfall.
 */
export function log(
  level: Level,
  event: string,
  message?: string,
  fields?: Fields,
  exchange?: Exchange,
): void {
  if (!enabled(level)) return
  const record: Record<string, unknown> = {
    ts: Date.now(),
    lvl: level,
    src: 'bridge',
    ev: event,
  }
  if (exchange !== undefined) {
    record['id'] = exchange.id
    record['org'] = exchange.origin
  }
  if (message !== undefined && message !== '') record['msg'] = message
  if (fields !== undefined && Object.keys(fields).length > 0) record['f'] = fields
  write(record)
}

export const error = (event: string, message?: string, fields?: Fields, x?: Exchange): void =>
  log('error', event, message, fields, x)
export const warn = (event: string, message?: string, fields?: Fields, x?: Exchange): void =>
  log('warn', event, message, fields, x)
export const info = (event: string, message?: string, fields?: Fields, x?: Exchange): void =>
  log('info', event, message, fields, x)
export const debug = (event: string, message?: string, fields?: Fields, x?: Exchange): void =>
  log('debug', event, message, fields, x)
export const trace = (event: string, message?: string, fields?: Fields, x?: Exchange): void =>
  log('trace', event, message, fields, x)

/**
 * Forward a line the harness host printed.
 *
 * Tagged `host` so it keeps its provenance in the log rather than reading as something
 * the bridge decided to say.
 */
export function host(line: string): void {
  if (THRESHOLD < 0) return
  write({ ts: Date.now(), lvl: 'info', src: 'host', ev: 'host.stdout', msg: scrubTokens(line) })
}

/**
 * Strip launch tokens out of a line of host output.
 *
 * The host's own stdout is arbitrary text and it prints URLs carrying a launch token. That
 * was harmless when it scrolled past on stderr; it is not harmless in a file that outlives
 * the session and is kept for twenty runs.
 */
export function scrubTokens(line: string): string {
  return line.replace(/([?&#](?:token|access_token|key|secret)=)[^\s&#]+/gi, '$1<redacted>')
}

function write(record: Record<string, unknown>): void {
  let line: string
  try {
    line = JSON.stringify(record)
  } catch {
    // A record that will not serialize must not take the process down, and must not
    // vanish either: say that one was lost and which event it was.
    line = JSON.stringify({ ts: Date.now(), lvl: 'warn', src: 'bridge', ev: 'log.unserializable',
      msg: String(record['ev']) })
  }
  process.stderr.write(`${PREFIX}${line}\n`)
}

// ── redaction ────────────────────────────────────────────────────────────────

/**
 * Keys whose values never reach the log, whatever the payload setting.
 *
 * Mirrors `SECRET_EXACT` in the Rust module. `token` is exact so token *counts* —
 * `inputTokens` — survive.
 */
const SECRET_EXACT = new Set([
  'token', 'accesstoken', 'access_token', 'refreshtoken', 'refresh_token',
  'apikey', 'api_key', 'secret', 'password', 'passwd', 'authorization',
  'credential', 'credentials', 'bearer', 'privatekey', 'private_key',
])

/** Substrings that make a key secret wherever they appear (`openaiApiKey`). */
const SECRET_CONTAINS = ['apikey', 'api_key', 'secret', 'password', 'credential']

function isSecretKey(key: string): boolean {
  const lower = key.toLowerCase()
  return SECRET_EXACT.has(lower) || SECRET_CONTAINS.some((needle) => lower.includes(needle))
}

/** Longest string kept verbatim when payloads are redacted. Mirrors `SHORT_STRING`. */
const SHORT_STRING = 48
const MAX_DEPTH = 8
const MAX_KEYS = 64
const MAX_ITEMS = 8
/** Serialized size above which a full payload is replaced by its shape. */
const MAX_BYTES = 64 * 1024

/**
 * Reduce a wire payload to what the log should hold.
 *
 * Full capture keeps the body, minus secrets and minus whatever exceeds the size cap;
 * otherwise the value is reduced to its shape — keys and types survive, long strings
 * become `str(N)` — so a flow stays readable without the prompts that produced it.
 */
export function describe(value: unknown): unknown {
  if (PAYLOADS) {
    const scrubbed = scrub(value, 0)
    let rendered: string
    try {
      rendered = JSON.stringify(scrubbed) ?? ''
    } catch {
      return shape(value, 0)
    }
    if (rendered.length > MAX_BYTES) {
      return { $elided: 'payload over cap', $bytes: rendered.length, $shape: shape(value, 0) }
    }
    return scrubbed
  }
  return shape(value, 0)
}

function scrub(value: unknown, depth: number): unknown {
  if (depth >= MAX_DEPTH) return '…'
  if (Array.isArray(value)) return value.map((item) => scrub(item, depth + 1))
  if (typeof value === 'object' && value !== null) {
    const out: Record<string, unknown> = {}
    for (const [key, child] of Object.entries(value)) {
      out[key] = isSecretKey(key) ? '<redacted>' : scrub(child, depth + 1)
    }
    return out
  }
  return value
}

function shape(value: unknown, depth: number): unknown {
  if (depth >= MAX_DEPTH) return '…'
  if (value === null) return 'null'
  if (value === undefined) return 'undefined'
  if (Array.isArray(value)) {
    const out: unknown[] = value.slice(0, MAX_ITEMS).map((item) => shape(item, depth + 1))
    if (value.length > MAX_ITEMS) out.push(`…+${value.length - MAX_ITEMS} more`)
    return out
  }
  switch (typeof value) {
    case 'string':
      return value.length <= SHORT_STRING ? value : `str(${value.length})`
    case 'number':
    case 'boolean':
      return value
    case 'object': {
      const entries = Object.entries(value as Record<string, unknown>)
      const out: Record<string, unknown> = {}
      for (const [key, child] of entries.slice(0, MAX_KEYS)) {
        out[key] = isSecretKey(key) ? '<redacted>' : shape(child, depth + 1)
      }
      if (entries.length > MAX_KEYS) out['…'] = `+${entries.length - MAX_KEYS} more keys`
      return out
    }
    default:
      return typeof value
  }
}
