/**
 * The dsh-tui wire protocol, TypeScript side.
 *
 * These declarations mirror `crates/dsh-tui-proto/src/lib.rs` exactly. Changing one
 * without the other breaks the handshake, so both cite PROTOCOL.md as the normative source.
 * @module
 */

/** Protocol major version. A client announcing a different major is refused. */
export const PROTOCOL_VERSION = 1

export type ExchangeId = number
export type Generation = number

/** Messages the TUI sends. */
export type ClientMsg =
  | { t: 'call'; id: ExchangeId; ns: string; m: string; args?: unknown }
  | { t: 'cancel'; id: ExchangeId }
  | { t: 'open'; id: ExchangeId; stream: string; args?: unknown }
  | { t: 'close'; id: ExchangeId }
  | { t: 'answer'; id: ExchangeId; v: unknown }
  | { t: 'next'; id: ExchangeId }
  | { t: 'reject'; id: ExchangeId; message: string }
  | { t: 'shutdown' }

/** Messages the bridge sends. */
export type ServerMsg =
  | { t: 'ready'; protocol: number; clientId?: string; host?: { home?: string }
      namespaces: string[]; events: string[] }
  | { t: 'ok'; id: ExchangeId; v: unknown }
  | { t: 'err'; id: ExchangeId; code: string; message: string; data?: unknown }
  | { t: 'item'; id: ExchangeId; gen: Generation; v: unknown }
  | { t: 'end'; id: ExchangeId; reason?: string }
  | { t: 'streamErr'; id: ExchangeId; code: string; message: string }
  | { t: 'event'; event: string; args: unknown[] }
  | { t: 'ask'; id: ExchangeId; event: string; agent?: unknown; args: unknown[] }
  | { t: 'bye' }

/** How a journal item relates to what the client already holds. */
export type JournalChange = 'replace' | 'prepend' | 'append'

/**
 * The forwarded-event allowlist this bridge expects, mirroring
 * `API_REMOTE_FORWARDED_EVENTS`. Asserted against the harness at boot so upstream drift
 * fails at startup rather than silently dropping a surface.
 */
export const EXPECTED_EVENTS = [
  'agent-preset/selected',
  'approval/request',
  'api-session/activity',
  'api-session/added',
  'api-session/error',
  'api-session/removed',
  'api-session/status',
  'commands/change',
  'credentials/reference-updated',
  'goal/activation-changed',
  'cordis/request-run',
  'cordis/request-run-resolved',
  'cordis/dynamic-package',
  'cordis/dynamic-retract',
  'cordis/inspect-query',
  'cordis/inspect-query-resolved',
  'llm/adapters-updated',
  'permission-presets/catalog-changed',
  'plugin-manager/changed',
  'plugin-manager/install-log',
  'plugin-manager/install-state',
  'settings/document-updated',
  'user-questions/request',
] as const

/** The two events that block the agent until the TUI answers. */
export const WATERFALL_EVENTS = ['approval/request', 'user-questions/request'] as const

export type WaterfallEvent = (typeof WATERFALL_EVENTS)[number]

export function isWaterfall(event: string): event is WaterfallEvent {
  return (WATERFALL_EVENTS as readonly string[]).includes(event)
}
