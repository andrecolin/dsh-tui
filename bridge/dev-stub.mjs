#!/usr/bin/env node
/**
 * A protocol-speaking stub of the bridge.
 *
 * It lets the Rust front end be developed and tested without a built harness, and it is
 * the fixture the Rust integration tests drive. It implements the wire contract only —
 * no Cordis, no harness — so a passing test proves protocol compatibility, not harness
 * behavior.
 *
 * Usage: dsh-tui --runtime node bridge/dev-stub.mjs
 */

const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`)

let themePreference = 'light'
let themeRevision = 9
let settingsRevision = 4
let settingsUser = { baseUrl: 'https://api.deepseek.com' }

const settingsDocument = () => ({
  writable: true,
  hasDocument: true,
  namespaces: [
    {
      ns: 'llm-deepseek',
      schema: { type: 'object', dict: {
        baseUrl: { type: 'string', meta: { description: 'API base URL' } },
        timeout: { type: 'number', meta: { description: 'Request timeout in seconds', min: 1, max: 600 } },
        stream: { type: 'boolean', meta: { description: 'Stream responses' } },
        mode: { type: 'union', list: [{ type: 'const', value: 'native' }, { type: 'const', value: 'ptc' }] },
        apiKey: { type: 'string', meta: { description: 'API key' } },
      } },
      value: {
        baseUrl: settingsUser.baseUrl ?? 'https://default', timeout: 30, stream: true, mode: 'native',
        providers: { gw: { apiKeyEnv: 'MY_GATEWAY_TOKEN' } },
      },
      user: { ...settingsUser, providers: { gw: { apiKeyEnv: 'MY_GATEWAY_TOKEN' } } },
      applies: 'live',
      secrets: [{ path: ['apiKey'], set: true }],
      revision: settingsRevision,
    },
    {
      ns: 'ui-theme',
      schema: { type: 'object', dict: {
        preference: { type: 'union', list: [{ type: 'const', value: 'light' }, { type: 'const', value: 'dark' }, { type: 'const', value: 'system' }] },
        fontSize: { type: 'number', meta: { min: 12, max: 17, step: 1 } },
      } },
      value: { preference: themePreference, fontSize: 14 },
      applies: 'live',
      secrets: [],
      revision: themeRevision,
    },
    {
      ns: 'tools',
      schema: { type: 'object', dict: { mode: { type: 'string', meta: { description: 'Tool presentation mode' } } } },
      value: { mode: 'native' },
      applies: 'restart',
      secrets: [],
      revision: 2,
    },
  ],
})

// The real `SessionSummary`: no title field; the display title is the `title` projection.
/**
 * One browse level, with the ancestry of the *listed* path.
 *
 * `crumbs` is the ancestor chain from the filesystem root to the listed directory
 * inclusive, which is what the real backend returns and what "go to parent" reads. A fixed
 * chain — which this stub used to return whatever the path was — makes `←` untestable: it
 * appears to work from one level and can never climb past it.
 */
function directoryListing(path) {
  const parts = path.split('/').filter(Boolean)
  const crumbs = [{ name: '/', path: '/', hidden: false }]
  let at = ''
  for (const part of parts) {
    at += `/${part}`
    crumbs.push({ name: part, path: at, hidden: false })
  }
  // Every level has children, so descending never runs out and `←` always has somewhere
  // to return to.
  const base = path === '/' ? '' : path
  return {
    path,
    home: '/home/acp',
    crumbs,
    entries: [
      { name: '.cache', path: `${base}/.cache`, hidden: true },
      { name: 'dsh-tui', path: `${base}/dsh-tui`, hidden: false },
      { name: 'harness', path: `${base}/harness`, hidden: false },
    ],
    truncated: true,
  }
}

/** The open `workspace.follow` stream, so created workspaces reach the client live. */
let workspaceStream = null

/** The workspace roster `workspace.follow` serves and `workspace.create` adds to. */
const WORKSPACES = [
  { workspaceId: 'w-1', path: '/home/acp/dsh-tui', title: 'dsh-tui', sessionIds: ['s-1', 's-2'], createdAt: '', updatedAt: '' },
  { workspaceId: 'w-2', path: '/home/acp/deepseek-harness', title: '', sessionIds: ['s-3'], createdAt: '', updatedAt: '' },
]

const SESSIONS = [
  { sessionId: 's-1', updatedAt: 3, running: true, blank: false, cwd: '/home/acp/dsh-tui',
    projections: { asOfSeq: 9, values: { title: 'Wire the trajectory pane' } } },
  { sessionId: 's-2', updatedAt: 2, running: false, blank: false,
    projections: { asOfSeq: 4, values: { title: 'Port the approval modal' } } },
  { sessionId: 's-3', updatedAt: 1, running: false, blank: true },
]

send({
  t: 'ready',
  protocol: 1,
  clientId: 'stub',
  host: { home: process.env.HOME ?? '/' },
  namespaces: [
    'session', 'skills', 'fileReferences', 'settings', 'credentials', 'workspace',
    'directoryPicker', 'commands', 'goal', 'agentPresets', 'subagent', 'llm',
    'pluginInventory', 'messageFeedback', 'sessionReference', 'cordisRunner',
  ],
  events: [
    'agent-preset/selected', 'approval/request', 'api-session/activity',
    'api-session/added', 'api-session/error', 'api-session/removed', 'api-session/status',
    'commands/change', 'credentials/reference-updated', 'cordis/request-run',
    'cordis/request-run-resolved', 'cordis/dynamic-package', 'cordis/dynamic-retract',
    'cordis/inspect-query', 'cordis/inspect-query-resolved', 'llm/adapters-updated',
    'settings/document-updated', 'user-questions/request',
  ],
})

// Exercised by the tests: a waterfall the front end must answer, delegate, or reject.
if (process.env.DSH_TUI_STUB_ASK === '1') {
  send({
    t: 'ask',
    id: 1,
    event: 'approval/request',
    agent: { sessionId: 's-1' },
    args: [{ title: 'Run `rm -rf build/`', toolName: 'bash' }],
  })
}

// A plan review: the approving option is listed second, so a UI reading option order
// would approve a decline.
if (process.env.DSH_TUI_STUB_ASK === 'plan') {
  send({
    t: 'ask',
    id: 5,
    event: 'user-questions/request',
    agent: { sessionId: 's-1' },
    args: [{ questions: [{
      id: 'plan-1',
      question: 'Approve this plan?',
      detail: '1. Wire the ledger\n2. Render the panes',
      options: [
        { label: 'Reject', description: 'Send it back' },
        { label: 'Approve', description: 'Proceed as written' },
      ],
      intent: { kind: 'plan-review', approve: 'Approve' },
    }] }],
  })
}

// A shape this build cannot render, to prove the front end delegates instead of deciding.
if (process.env.DSH_TUI_STUB_ASK === 'opaque') {
  send({ t: 'ask', id: 2, event: 'approval/request', args: [{ somethingNew: true }] })
}

let streamTimer
const openStreams = new Set()

process.stdin.setEncoding('utf8')
let buffer = ''
process.stdin.on('data', (chunk) => {
  buffer += chunk
  let newline
  while ((newline = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, newline)
    buffer = buffer.slice(newline + 1)
    if (line.trim() !== '') handle(JSON.parse(line))
  }
})

function handle(msg) {
  switch (msg.t) {
    case 'call': {
      if (msg.ns === 'session' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: { items: SESSIONS } })
      } else if (msg.ns === 'session' && msg.m === 'prompt') {
        // The real receipt is `{ accepted: true }`; the request carries a client-minted id.
        const request = (msg.args && msg.args.request) || {}
        if (typeof request.requestId !== 'string' || !Array.isArray(request.content)) {
          send({ t: 'err', id: msg.id, code: 'bad-request', message: 'prompt needs requestId and content' })
          return
        }
        // `DSH_TUI_STUB_REJECT_PROMPT=1` plays a host that will not take the message:
        // the case where the composer empties and nothing else happens.
        if (process.env.DSH_TUI_STUB_REJECT_PROMPT === '1') {
          send({ t: 'err', id: msg.id, code: 'unavailable', message: 'the agent is not accepting messages' })
          return
        }
        send({ t: 'ok', id: msg.id, v: { accepted: true } })
        send({ t: 'event', event: 'api-session/status', args: [{ sessionId: 's-1', status: 'running' }] })
      } else if (msg.ns === 'session' && msg.m === 'create') {
        if (process.env.DSH_TUI_STUB_REJECT_CREATE === '1') {
          send({ t: 'err', id: msg.id, code: 'unavailable', message: 'no agent slots free' })
          return
        }
        const request = (msg.args && msg.args.request) || {}
        // The host's own rule, mirrored so a client that sends both is caught here rather
        // than in the field: `session.create` takes one locator, not two.
        if (request.workspaceId !== undefined && request.cwd !== undefined) {
          send({ t: 'err', id: msg.id, code: 'bad-request',
                 message: 'session.create accepts workspaceId or cwd, not both' })
          return
        }
        if (request.workspaceId !== undefined
            && !WORKSPACES.some((ws) => ws.workspaceId === request.workspaceId)) {
          send({ t: 'err', id: msg.id, code: 'workspace-not-found',
                 message: `workspace "${request.workspaceId}" not found` })
          return
        }
        const created = `s-${SESSIONS.length + 1}`
        // The host resolves the directory from the workspace when given its id, and
        // attaches the session to it — which is what `cwd` alone never does.
        const byId = WORKSPACES.find((ws) => ws.workspaceId === request.workspaceId)
        const cwd = byId?.path ?? request.cwd
        SESSIONS.push({ sessionId: created, updatedAt: 9, running: false, blank: true, cwd })
        send({ t: 'ok', id: msg.id, v: { sessionId: created, agentPreset: 'standard' } })
        // The host accounts a new session to the workspace it was created in, and pushes
        // the changed row onto the live feed. Without that the client sees a workspace
        // that still has no sessions and cannot tell the new one belongs to it.
        // `DSH_TUI_STUB_NO_ACCOUNTING=1` plays the host seen in the field: it creates the
        // session with the right `cwd` but never adds it to the workspace's roster.
        // Attachment follows the workspace id, exactly as the host does it: a request
        // that named only a directory joins no workspace.
        const owner = process.env.DSH_TUI_STUB_NO_ACCOUNTING === '1' ? undefined : byId
        if (owner !== undefined) {
          owner.sessionIds = [...owner.sessionIds, created]
          if (workspaceStream !== null) {
            send({ t: 'item', id: workspaceStream, gen: 1,
                   v: { type: 'upsert', workspace: owner } })
          }
        }
      } else if (msg.ns === 'workspace' && msg.m === 'create') {
        const path = ((msg.args && msg.args.request) || {}).path
        if (typeof path !== 'string' || path === '') {
          send({ t: 'err', id: msg.id, code: 'bad-request', message: 'create needs a path' })
          return
        }
        // `DSH_TUI_STUB_NO_WORKSPACE_CREATE=1` plays a harness that does not expose the
        // method, so the client's fallback is exercised rather than assumed.
        if (process.env.DSH_TUI_STUB_NO_WORKSPACE_CREATE === '1') {
          send({ t: 'err', id: msg.id, code: 'not-found', message: 'unknown method workspace.create' })
          return
        }
        const existing = WORKSPACES.find((ws) => ws.path === path)
        if (existing !== undefined) {
          send({ t: 'ok', id: msg.id, v: { workspace: existing, created: false } })
          return
        }
        const made = {
          workspaceId: `w-${WORKSPACES.length + 1}`,
          path,
          title: path.split('/').filter(Boolean).pop() ?? path,
          sessionIds: [],
          createdAt: '',
          updatedAt: '',
        }
        WORKSPACES.push(made)
        send({ t: 'ok', id: msg.id, v: { workspace: made, created: true } })
        if (workspaceStream !== null) {
          send({ t: 'item', id: workspaceStream, gen: 1, v: { type: 'upsert', workspace: made } })
        }
      } else if (msg.ns === 'settings' && msg.m === 'describe') {
        send({ t: 'ok', id: msg.id, v: settingsDocument() })
      } else if (msg.ns === 'settings' && msg.m === 'mutate' && msg.args && msg.args.ns === 'ui-theme') {
        const { ops, expectedRevision } = msg.args
        if (expectedRevision !== themeRevision) {
          send({ t: 'err', id: msg.id, code: 'conflict', message: `revision ${expectedRevision} is stale (now ${themeRevision})` })
          return
        }
        for (const op of ops ?? []) if (op.op === 'set' && op.path[0] === 'preference') themePreference = op.value
        themeRevision += 1
        send({ t: 'ok', id: msg.id, v: { ns: 'ui-theme', revision: themeRevision } })
      } else if (msg.ns === 'settings' && msg.m === 'mutate') {
        const { ops, expectedRevision } = msg.args ?? {}
        if (expectedRevision !== settingsRevision) {
          // The real controller refuses a stale editor rather than clobbering a
          // concurrent change; the TUI must surface that, not retry blindly.
          send({ t: 'err', id: msg.id, code: 'conflict', message: `revision ${expectedRevision} is stale (now ${settingsRevision})` })
          return
        }
        for (const op of ops ?? []) {
          if (op.op === 'set') settingsUser[op.path.join('.')] = op.value
          else delete settingsUser[op.path.join('.')]
        }
        settingsRevision += 1
        send({ t: 'ok', id: msg.id, v: { ns: msg.args.ns, revision: settingsRevision } })
      } else if (msg.ns === 'directoryPicker' && msg.m === 'list') {
        const path = (msg.args && msg.args.path) || '/home/acp/projects'
        // `DSH_TUI_STUB_PICKER_FLOOR` plays a host that will not browse above a root:
        // the case where `←` appears to do nothing at all.
        const floor = process.env.DSH_TUI_STUB_PICKER_FLOOR
        if (floor !== undefined && !path.startsWith(floor)) {
          send({ t: 'err', id: msg.id, code: 'permission-denied',
                 message: `${path} is outside the browsable roots` })
          return
        }
        send({ t: 'ok', id: msg.id, v: directoryListing(path) })
      } else if (msg.ns === 'agentPresets' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: { authorable: true, presets: [
          { id: 'coding', trust: 'system', isDefault: true, name: 'Coding', description: 'General coding agent' },
          { id: 'reviewer', trust: 'user', isDefault: false, name: 'Reviewer' },
          { id: 'legacy', trust: 'user', isDefault: false, broken: 'missing tool: dsh-tool-bash' },
        ] } })
      } else if (msg.ns === 'skills' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: { skills: [
          { name: 'code-review', description: 'Review the current diff', modelInvocable: true },
          { name: 'simplify', description: 'Apply quality cleanups', whenToUse: 'after a refactor', modelInvocable: false },
        ] } })
      } else if (msg.ns === 'subagent' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: { parentAvailable: true, entries: [
          { kind: 'child', id: 'sub-1', activity: 'running', hasChildren: false, mode: 'continuable', label: 'reviewer' },
          { kind: 'child', id: 'sub-2', activity: 'inactive', hasChildren: true, mode: 'one-shot' },
          { kind: 'diagnostic', id: 'sub-3', reason: 'unavailable' },
        ] } })
      } else if (msg.ns === 'session' && msg.m === 'page') {
        // Matches the real descriptor: `session/page(request)`.
        const { throughSeq, beforeSeq } = (msg.args && msg.args.request) || {}
        if (throughSeq !== 100) {
          // A page not quoting the follow frame's cut would slide as live events append.
          send({ t: 'err', id: msg.id, code: 'bad-request', message: `throughSeq ${throughSeq} is not the follow cut` })
          return
        }
        const before = beforeSeq ?? 1
        const from = Math.max(1, before - 5)
        const records = []
        for (let seq = from; seq < before; seq += 1) {
          records.push({ type: 'event', event: { type: 'assistant/message', seq, time: seq * 10, data: { content: [{ type: 'text', text: `older line ${seq}` }] } } })
        }
        send({ t: 'ok', id: msg.id, v: { records, hasMore: from > 1 } })
      } else if (msg.ns === 'session' && msg.m === 'modelCatalog') {
        send({ t: 'ok', id: msg.id, v: {
          default: { provider: 'deepseek-official', model: 'deepseek-v4-pro' },
          routableProviders: ['deepseek-official', 'anthropic', 'empty-gw'],
          groups: [
            { id: 'deepseek-official', name: 'DeepSeek', models: [
              { id: 'deepseek-v4-pro', name: 'V4 Pro' },
              { id: 'deepseek-v4' },
            ] },
            { id: 'empty-gw', name: 'Empty Gateway', models: [] },
          ],
          failures: [{ id: 'anthropic', name: 'Anthropic', message: '401 unauthorized' }],
        } })
      } else if (msg.ns === 'session' && msg.m === 'selectModel') {
        send({ t: 'ok', id: msg.id, v: { selected: msg.args.request } })
      } else if (msg.ns === 'pluginInventory' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: { entries: [
          { entryId: 'e1', moduleName: '@deepseek-ai/dsh-tool-bash', enabled: true, fiberPhase: 'active' },
          { entryId: 'e2', moduleName: '@deepseek-ai/dsh-tool-terminal', enabled: true, fiberPhase: 'active' },
          { entryId: 'e3', moduleName: '@deepseek-ai/dsh-web-search-exa', enabled: false, fiberPhase: null },
          { entryId: 'e4', moduleName: '@deepseek-ai/dsh-goal', enabled: true, fiberPhase: 'failed' },
          { entryId: 'e5', moduleName: '@acp/dsh-tui-bridge', enabled: true, fiberPhase: null },
          { entryId: 'e6', moduleName: '@deepseek-ai/dsh-schedule', enabled: true, fiberPhase: 'quiescing' },
        ] } })
      } else if (msg.ns === 'llm' && msg.m === 'listProviders') {
        send({ t: 'ok', id: msg.id, v: [
          { id: 'deepseek-official', name: 'DeepSeek' },
          { id: 'anthropic', name: 'Anthropic' },
        ] })
      } else if (msg.ns === 'llm' && msg.m === 'listConfigurableProviders') {
        send({ t: 'ok', id: msg.id, v: [
          { provider: 'deepseek-official', displayName: 'DeepSeek', settingsNs: 'llm-deepseek', settingsPath: [] },
          { provider: 'anthropic', displayName: 'Anthropic', settingsNs: 'llm-deepseek', settingsPath: ['providers', 'anthropic'] },
          { provider: 'my-gateway', displayName: 'My Gateway', settingsNs: 'llm-deepseek', settingsPath: ['providers', 'gw'], declared: true },
        ] })
      } else if (msg.ns === 'credentials' && msg.m === 'describe') {
        if (process.env.DSH_TUI_STUB_CRED_FAIL === '1') {
          send({ t: 'err', id: msg.id, code: 'internal', message: 'credential provider unavailable' })
          return
        }
        const refs = (msg.args && msg.args.refs) || []
        const known = {
          DEEPSEEK_OFFICIAL_API_KEY: { configured: true, source: 'env', writable: true },
          MY_GATEWAY_TOKEN: { configured: false, writable: true },
        }
        send({ t: 'ok', id: msg.id, v: Object.fromEntries(refs.filter((r) => r in known).map((r) => [r, known[r]])) })
      } else if (msg.ns === 'commands' && msg.m === 'list') {
        send({ t: 'ok', id: msg.id, v: [
          { name: 'model', description: 'Choose the conversation model' },
          { name: 'permission', description: 'Choose a permission preset' },
          { name: 'export', description: 'Export this session' },
          { name: 'compact', description: 'Compact the transcript' },
        ] })
      } else if (msg.ns === 'fileReferences' && msg.m === 'list') {
        const query = (msg.args && msg.args.query) || ''
        const all = ['src/lex.rs', 'src/parse.rs', 'src/main.rs', 'README.md']
        send({ t: 'ok', id: msg.id, v: all.filter((p) => p.includes(query)).map((path) => ({ path })) })
      } else {
        send({ t: 'err', id: msg.id, code: 'not-found', message: `no ${msg.ns}.${msg.m}` })
      }
      return
    }
    case 'open': {
      openStreams.add(msg.id)
      if (msg.stream === 'session.control') {
        send({ t: 'item', id: msg.id, gen: 1, v: { type: 'baseline', value: {
          queues: {},
          jobs: { 's-1': [
            { id: 'j1', kind: 'bash', label: 'cargo test', status: 'running', startedAt: 1 },
            { id: 'j2', kind: 'bash', label: 'old build', status: 'completed', startedAt: 1, finishedAt: 2 },
          ] },
          projections: { 's-1': {
            plan: { active: true, pending: false },
            goal: { objective: 'Ship the TUI', phase: 'active', activation: 'disarmed', maxGoalRounds: 20, roundsStarted: 3 },
            imageLimits: { maxImageBytes: 1048576, maxImagesPerMessage: 2, maxMessageImageBytes: 1572864, maxImagePixels: 3000000, maxImageDimension: 2000, mediaTypes: ['image/png', 'image/jpeg'] },
            permissions: { currentValue: 'custom', options: [
              { value: 'safe', name: 'Safe', description: 'Ask before acting' },
              { value: 'yolo', name: 'Yolo' },
              { value: 'custom', name: 'Custom' },
            ] },
          } },
        } } })
        return
      }
      if (msg.stream === 'workspace.follow') {
        // Remembered so `workspace.create` can push the new row onto the live feed, the
        // way the host does. Without it a created workspace never reaches the client and
        // the client cannot tell it apart from a path the host has never heard of.
        workspaceStream = msg.id
        send({ t: 'item', id: msg.id, gen: 1, v: { type: 'baseline', value: {
          items: WORKSPACES,
          archivedSessionIds: ['s-2'],
        } } })
        return
      }
      // A baseline that looks like a real transcript: a user turn, a streamed answer
      // packed into a chunk row, a tool call, and its result.
      send({
        t: 'item', id: msg.id, gen: 1,
        v: {
          change: 'replace',
          // The inclusive cut a backwards page must quote, and whether older history exists.
          cursor: 100,
          hasMore: true,
          records: [
            { type: 'event', event: { type: 'turn/start', seq: 21, time: 1000, data: {} } },
            { type: 'event', event: { type: 'user/message', seq: 22, time: 1010, data: { content: [{ type: 'text', text: 'why is the parser dropping the last token?' }] } } },
            { type: 'chunks', event: { type: 'chunkrow/text-chunks', seq: 23, time: 1200, data: { turn: 1, step: 0, index: 0, dt: [8, 9, 7], texts: ['Checking the ', 'tokenizer ', 'bounds.'] } } },
            { type: 'event', event: { type: 'tool/call', seq: 26, time: 1400, data: { turn: 1, step: 0, callId: 'c1', name: 'read_file', arguments: JSON.stringify({ path: 'src/lex.rs' }) } } },
            { type: 'event', event: { type: 'tool/result', seq: 27, time: 2650, data: { turn: 1, step: 0, message: { content: [{ toolCallId: 'c1', content: [{ type: 'text', text: '182 lines read\nfirst: use std::str;' }], isError: false }] } } } },
            { type: 'event', event: { type: 'tool/call', seq: 28, time: 2700, data: { turn: 1, step: 0, callId: 'c2', name: 'bash', arguments: '{"command":"cargo te' } } },
            { type: 'event', event: { type: 'tool/result', seq: 29, time: 3100, data: { turn: 1, step: 0, message: { content: [{ toolCallId: 'c2', content: [], isError: false }] }, error: { name: 'ParseError', code: 'bad-arguments' } } } },
            { type: 'event', event: { type: 'tool/call', seq: 30, time: 3150, data: { turn: 1, step: 0, callId: 'root1', name: 'run_code', arguments: JSON.stringify({ code: 'await bash("ls")' }) } } },
            { type: 'event', event: { type: 'tool/code-dispatch-start', seq: 31, time: 3160, data: { rootCallId: 'root1', parentCallId: 'root1', subCallId: 'root1:code:1', name: 'bash', arguments: { command: 'ls -la' } } } },
            { type: 'event', event: { type: 'tool/code-dispatch', seq: 32, time: 3260, data: { rootCallId: 'root1', parentCallId: 'root1', subCallId: 'root1:code:1', name: 'bash', arguments: { command: 'ls -la' }, isError: false, content: [{ type: 'text', text: '12 entries' }] } } },
            { type: 'event', event: { type: 'tool/code-dispatch-start', seq: 33, time: 3270, data: { rootCallId: 'root1', parentCallId: 'root1:code:1', subCallId: 'root1:code:2', name: 'read_file', arguments: { path: 'src/lex.rs' } } } },
            { type: 'event', event: { type: 'tool/result', seq: 34, time: 3300, data: { turn: 1, step: 0, message: { content: [{ toolCallId: 'root1', content: [], isError: false }] } } } },
            { type: 'event', event: { type: 'turn/end', seq: 35, time: 3400, data: {} } },
            { type: 'event', event: { type: 'turn/start', seq: 36, time: 3500, data: {} } },
            { type: 'event', event: { type: 'tool-workflow/run-start', seq: 37, time: 3510, data: { runId: 'wf1', name: 'review-changes' } } },
            { type: 'event', event: { type: 'tool-workflow/agent-start', seq: 38, time: 3520, data: { runId: 'wf1', seq: 21, label: 'review:bugs', phase: 'Review', childId: 'k1' } } },
            { type: 'event', event: { type: 'tool-workflow/agent-end', seq: 39, time: 3900, data: { runId: 'wf1', seq: 21, outcome: 'completed' } } },
            { type: 'event', event: { type: 'tool/call', seq: 40, time: 3950, data: { turn: 2, step: 0, callId: 'c3', name: 'write', arguments: JSON.stringify({ file_path: 'src/parse.rs', content: 'fn parse(){}' }) } } },
            { type: 'event', event: { type: 'tool/result', seq: 41, time: 4000, data: { turn: 2, step: 0, message: { content: [{ toolCallId: 'c3', content: [], isError: false }] } } } },
            { type: 'event', event: { type: 'tool/call', seq: 42, time: 4010, data: { turn: 2, step: 0, callId: 'c4', name: 'edit', arguments: JSON.stringify({ file_path: 'src/nochange.rs', old_string: 'x', new_string: 'x' }) } } },
            { type: 'event', event: { type: 'tool/result', seq: 43, time: 4020, data: { turn: 2, step: 0, message: { content: [{ toolCallId: 'c4', content: [], isError: false }] } } } },
          ],
        },
      })
      let seq = 44
      streamTimer = setInterval(() => {
        if (!openStreams.has(msg.id)) return
        send({
          t: 'item', id: msg.id, gen: 1,
          v: {
            change: 'append',
            records: [{ type: 'chunks', event: { type: 'chunkrow/text-chunks', seq, time: Date.now(), data: { turn: 1, step: 1, index: 0, dt: [5], texts: [' The bound is ', 'off by one.'] } } }],
          },
        })
        seq += 2
      }, 1200)
      streamTimer.unref?.()
      return
    }
    case 'close': {
      openStreams.delete(msg.id)
      send({ t: 'end', id: msg.id, reason: 'disposed' })
      return
    }
    case 'answer':
    case 'next':
    case 'reject': {
      // Echo the resolution so a test can assert which reply arrived.
      process.stderr.write(`stub: waterfall ${msg.id} resolved via ${msg.t}\n`)
      return
    }
    case 'shutdown': {
      send({ t: 'bye' })
      clearInterval(streamTimer)
      process.exit(0)
    }
  }
}
