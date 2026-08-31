//! Application state and the reducer over protocol messages and key events.

use std::collections::VecDeque;

use std::collections::HashMap;

use dsh_tui_proto::{ClientMsg, ExchangeId, JournalItem, Ready, ServerMsg};
use serde_json::Value;

use crate::attachment::{self, Draft, Graphics, Limits};
use crate::chips::{Catalog, Goal, PlanChip};
use crate::directory::{Browser, Listing};
use crate::image::{self, CellRect, Placement};
use crate::locale::Locale;
use crate::logging::{self, Level, Record};
use crate::composer::{Composer, TriggerKind};
use crate::control::{Control, Frame as ControlFrame};
use crate::models::{self, ConfigurableProvider, CredentialInfo, ProviderInfo, ProviderRow};
use crate::plugins::{Inventory, Snapshot as PluginSnapshot};
use crate::presets::{AgentPresetRoster, PermissionSelect};
use crate::questions::{Draft as AnswerDraft, Request as QuestionRequest};
use crate::session::{Apply, Ledger};
use crate::subagent::Catalog as SubagentCatalog;
use crate::settings::{self, Describe, Field};
use crate::theme::{self, Preference, Theme};
use crate::viewport::{self, Viewport};
use crate::workspace::{Frame as WorkspaceFrame, WorkspaceRow, Workspaces};
use crate::transport::{Incoming, Transport};

/// How many records the in-app tail keeps. The log file keeps everything.
const LOG_TAIL: usize = 2000;

/// This run's log file as a JSON field, or null when logging is off.
fn path_field() -> Value {
    logging::path()
        .map(|path| Value::from(path.display().to_string()))
        .unwrap_or(Value::Null)
}

/// The three columns of the web client's `AppFrame`, and the focus ring over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Conversation,
    Details,
}

impl Pane {
    /// Cycle focus, skipping panes that are currently collapsed.
    pub fn next(self, sidebar_open: bool, details_open: bool) -> Self {
        let ring = [Pane::Sidebar, Pane::Conversation, Pane::Details];
        let start = ring.iter().position(|p| *p == self).unwrap_or(1);
        for step in 1..=ring.len() {
            let candidate = ring[(start + step) % ring.len()];
            let visible = match candidate {
                Pane::Sidebar => sidebar_open,
                Pane::Details => details_open,
                Pane::Conversation => true,
            };
            if visible {
                return candidate;
            }
        }
        Pane::Conversation
    }
}

/// What the centre column is showing. The web client gives settings and the workspace
/// browser their own surfaces; in a terminal they take the main column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Conversation,
    Settings,
    Workspace,
    /// This run's log. A TUI cannot print diagnostics to the terminal it owns, so the
    /// only way to watch the flow without leaving the app is to give it a surface.
    Logs,
}

/// Which page the settings surface shows. The web client splits these into sections; the
/// terminal switches between them with Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    /// The generic namespace form.
    Namespaces,
    /// The Models page: the provider directory joined to settings and credentials.
    Models,
    /// The read-only Loader inventory.
    Plugins,
    /// Appearance and other ownerless product rows.
    General,
}

/// Handshake state.
#[derive(Debug, Clone)]
pub enum Connection {
    Connecting,
    /// Payload read once the settings and workspace panes consume host facts.
    Ready(#[allow(dead_code)] Box<Ready>),
    Failed(String),
}

/// One row of the session list.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    /// Whether the session is live right now.
    pub running: bool,
    /// Working directory, when the summary carries one.
    pub cwd: Option<String>,
    /// Listed but not a top-level conversation.
    pub is_subagent: bool,
}

/// A waterfall the harness is blocked on.
#[derive(Debug, Clone)]
pub struct PendingAsk {
    pub id: ExchangeId,
    pub event: String,
    pub args: Vec<Value>,
}

impl PendingAsk {
    /// Best-effort human summary of the request, without pretending to understand a shape
    /// this build does not know.
    pub fn summary(&self) -> String {
        let first = self.args.first();
        let described = first.and_then(|value| {
            value
                .get("title")
                .or_else(|| value.get("question"))
                .or_else(|| value.get("toolName"))
                .or_else(|| value.get("summary"))
                .and_then(Value::as_str)
        });
        match described {
            Some(text) => text.to_string(),
            None => format!("{} (unrecognized shape)", self.event),
        }
    }

    /// Whether this build can render the request well enough to ask a human about it.
    ///
    /// When it cannot, the only correct reply is `next`: delegating lets the host's own
    /// listener decide, whereas answering would invent a decision on the user's behalf.
    pub fn is_renderable(&self) -> bool {
        self.args
            .first()
            .map(|value| {
                value.get("title").is_some()
                    || value.get("question").is_some()
                    || value.get("toolName").is_some()
                    || value.get("summary").is_some()
            })
            .unwrap_or(false)
    }
}

pub struct App {
    pub theme: Theme,
    pub connection: Connection,
    pub sessions: Vec<SessionRow>,
    pub selected_session: usize,
    pub asks: VecDeque<PendingAsk>,
    /// Tail of this run's log, mirroring what the log file received.
    pub log: Vec<Record>,
    /// Substring the log pane is filtered to. Matches messages, events and fields, so an
    /// exchange id pulls up both halves of one exchange.
    pub log_filter: String,
    /// The log pane's own threshold. Starts at the file's, and can only narrow: a record
    /// the file never received cannot be shown here.
    pub log_min: Level,
    pub log_scroll: Viewport,
    /// The directory new sessions are rooted in, once the human has chosen one.
    ///
    /// There is deliberately no default. An agent's working directory decides where its
    /// edits and deliverables land, so inferring one from the process cwd means files
    /// appear wherever the binary happened to be launched from — which is how this
    /// repository ended up with another project's documents in it.
    pub workspace: Option<String>,
    /// A session creation waiting on the workspace being chosen.
    pending_new_session: bool,
    /// Cursor into the `^w` workspace list.
    pub workspace_row: usize,
    /// Rows visible in the directory picker, measured each frame.
    pub picker_page: usize,
    /// Why the last browse attempt went nowhere. Shown in the dialog: a refused or
    /// dropped navigation that says nothing is indistinguishable from a broken key.
    pub picker_error: Option<String>,
    /// How many levels above the listed directory the pending climb has reached.
    picker_climb: usize,
    /// A prompt typed before this workspace had a session, waiting for one.
    pending_prompt: Option<String>,
    /// The host's id for the current workspace, when it has told us one.
    workspace_id: Option<String>,
    /// In-flight `session.prompt`, so its rejection can be shown rather than logged.
    prompt_call: Option<ExchangeId>,
    /// Why the last prompt did not go out. Shown beside the composer.
    pub prompt_error: Option<String>,
    /// In flight `workspace.create`, and the path it is registering.
    workspace_create: Option<(ExchangeId, String)>,
    pub focus: Pane,
    pub sidebar_open: bool,
    pub details_open: bool,
    pub composer: Composer,
    /// Candidates for the trigger under the caret.
    pub candidates: Vec<Candidate>,
    pub candidate_index: usize,
    pub should_quit: bool,
    /// Records for the session being followed.
    pub ledger: Ledger,
    /// Which surface the centre column shows.
    pub view: View,
    /// The settings document, once read.
    pub settings: Option<Describe>,
    pub settings_ns: usize,
    pub settings_field: usize,
    /// Last write failure, shown beside the form.
    pub settings_error: Option<String>,
    /// Which settings page is showing.
    pub settings_section: SettingsSection,
    /// Routes the adapter registry has registered.
    pub providers: Vec<ProviderInfo>,
    /// Routes configuration can activate.
    pub configurable: Vec<ConfigurableProvider>,
    /// Credential state, keyed by reference.
    pub credentials: HashMap<String, CredentialInfo>,
    /// Set when credential lookup failed; the page still renders without it.
    pub credential_error: Option<String>,
    pub models_row: usize,
    /// The Loader inventory and its search.
    pub inventory: Inventory,
    pub inventory_row: usize,
    /// The stored theme preference, and how it resolved.
    pub theme_preference: Preference,
    pub theme_reason: &'static str,
    /// Terminal-reported background, read once at boot.
    colorfgbg: Option<String>,
    /// Live control state: queues, jobs, and projections for every session.
    pub control: Control,
    /// The model catalog, once read.
    pub catalog: Catalog,
    /// Whether the model picker is open, and its query.
    pub model_picker: Option<String>,
    pub model_row: usize,
    /// Direct children of the selected session.
    pub subagents: SubagentCatalog,
    /// The agent-preset roster, once read.
    pub preset_roster: AgentPresetRoster,
    /// The workspace directory browser, when open.
    pub picker: Option<Browser>,
    /// Images staged for the next message.
    pub drafts: Vec<Draft>,
    /// Last attachment rejection, shown beside the composer.
    pub attachment_error: Option<String>,
    /// What this terminal can draw.
    pub graphics: Graphics,
    /// The active locale, read from the environment at boot.
    pub locale: Locale,
    /// Conversation scrollback.
    pub scroll: Viewport,
    /// The conversation search query, when the search bar is open.
    pub search: Option<String>,
    /// Line indices matching the query, and which one is current.
    pub search_hits: Vec<usize>,
    pub search_index: usize,
    /// Rendered conversation lines from the last frame, for search and scrolling.
    pub rendered: Vec<String>,
    /// The question request taking over the composer, if one is pending.
    pub questions: Option<PendingQuestions>,
    /// Workspace browser state.
    pub workspaces: Workspaces,
    /// The in-flight `session.list` call, so its reply can be told apart from others.
    session_list_call: Option<ExchangeId>,
    /// The in-flight candidate lookup, and the query it was issued for.
    candidate_call: Option<ExchangeId>,
    candidate_query: Option<(TriggerKind, String)>,
    /// The in-flight `settings.describe` call.
    settings_call: Option<ExchangeId>,
    /// The in-flight settings write.
    settings_write: Option<ExchangeId>,
    /// In-flight Models page lookups.
    providers_call: Option<ExchangeId>,
    configurable_call: Option<ExchangeId>,
    credentials_call: Option<ExchangeId>,
    inventory_call: Option<ExchangeId>,
    /// The open `workspace.follow` stream.
    workspace_stream: Option<ExchangeId>,
    /// The open `session.control` stream.
    control_stream: Option<ExchangeId>,
    catalog_call: Option<ExchangeId>,
    skills_call: Option<ExchangeId>,
    subagents_call: Option<ExchangeId>,
    presets_call: Option<ExchangeId>,
    picker_call: Option<ExchangeId>,
    /// The in-flight backwards page.
    page_call: Option<ExchangeId>,
    /// The in-flight session creation, and the session it produced.
    create_call: Option<ExchangeId>,
    pending_selection: Option<String>,
    /// The open `session.follow` stream, if any.
    follow_stream: Option<ExchangeId>,
    /// Which session `follow_stream` is bound to.
    followed_session: Option<String>,
}

impl App {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            connection: Connection::Connecting,
            sessions: Vec::new(),
            selected_session: 0,
            asks: VecDeque::new(),
            log: Vec::new(),
            log_filter: String::new(),
            log_min: logging::level(),
            log_scroll: Viewport::new(),
            workspace: None,
            pending_new_session: false,
            workspace_row: 0,
            picker_page: 10,
            picker_error: None,
            picker_climb: 0,
            pending_prompt: None,
            workspace_id: None,
            prompt_call: None,
            prompt_error: None,
            workspace_create: None,
            focus: Pane::Conversation,
            sidebar_open: true,
            details_open: false,
            composer: Composer::new(),
            candidates: Vec::new(),
            candidate_index: 0,
            should_quit: false,
            ledger: Ledger::new(),
            view: View::Conversation,
            settings: None,
            settings_ns: 0,
            settings_field: 0,
            settings_error: None,
            settings_section: SettingsSection::Namespaces,
            providers: Vec::new(),
            configurable: Vec::new(),
            credentials: HashMap::new(),
            credential_error: None,
            models_row: 0,
            inventory: Inventory::new(),
            inventory_row: 0,
            theme_preference: Preference::Dark,
            theme_reason: "set to dark",
            colorfgbg: std::env::var("COLORFGBG").ok(),
            control: Control::new(),
            catalog: Catalog::default(),
            model_picker: None,
            model_row: 0,
            subagents: SubagentCatalog::default(),
            preset_roster: AgentPresetRoster::default(),
            picker: None,
            drafts: Vec::new(),
            attachment_error: None,
            graphics: Graphics::from_env(),
            locale: Locale::from_env(),
            scroll: Viewport::new(),
            search: None,
            search_hits: Vec::new(),
            search_index: 0,
            rendered: Vec::new(),
            questions: None,
            workspaces: Workspaces::new(),
            session_list_call: None,
            candidate_call: None,
            candidate_query: None,
            settings_call: None,
            settings_write: None,
            providers_call: None,
            configurable_call: None,
            credentials_call: None,
            inventory_call: None,
            workspace_stream: None,
            control_stream: None,
            catalog_call: None,
            skills_call: None,
            subagents_call: None,
            presets_call: None,
            picker_call: None,
            page_call: None,
            create_call: None,
            pending_selection: None,
            follow_stream: None,
            followed_session: None,
        }
    }

    /// Record a message from the UI half.
    ///
    /// Almost every caller is an error path — a send that failed, a frame that did not
    /// match its contract — so the default level is `warn`. Anything that wants a
    /// different level or structured fields builds a [`Record`] and calls [`Self::record`].
    fn push_log(&mut self, line: impl Into<String>) {
        self.record(Record::warn("app").msg(line));
    }

    /// Write a record to the log file and keep it in the pane's tail.
    ///
    /// A record the file filtered out is dropped here too, so the pane never shows a run
    /// the file does not contain.
    pub fn record(&mut self, record: Record) {
        let Some(record) = logging::emit(record) else { return };
        self.adopt(record);
    }

    /// Keep a record that has *already* been written to the file.
    ///
    /// Everything arriving as [`Incoming::Note`] — the transport's own frames, and every
    /// record the bridge and the harness host produced — was written where it was
    /// created. Emitting it again here would put every one of them in the file twice.
    fn adopt(&mut self, record: Record) {
        self.log.push(record);
        // The pane is a tail, not an archive — the file is the archive.
        if self.log.len() > LOG_TAIL {
            self.log.drain(..self.log.len() - LOG_TAIL);
        }
    }

    /// Fold one message from the child into state.
    pub fn on_incoming(&mut self, incoming: Incoming, transport: &mut Transport) {
        match incoming {
            Incoming::Msg(msg) => self.on_message(*msg, transport),
            Incoming::Note(record) => self.adopt(record),
            Incoming::Malformed { line, error } => {
                let preview: String = line.chars().take(200).collect();
                self.record(
                    Record::error("frame.malformed")
                        .msg(format!("skipped a frame that did not parse: {error}"))
                        .field("error", error)
                        .field("bytes", line.len())
                        .field("preview", preview),
                );
            }
            Incoming::Exited { status } => {
                let detail = match status {
                    Some(code) => format!("harness runtime exited with status {code}"),
                    None => "harness runtime exited".to_string(),
                };
                if !matches!(self.connection, Connection::Failed(_)) {
                    self.connection = Connection::Failed(detail.clone());
                }
                self.record(
                    Record::error("transport.exit")
                        .msg(detail)
                        .field("status", status.map(Value::from).unwrap_or(Value::Null)),
                );
            }
        }
    }

    fn on_message(&mut self, msg: ServerMsg, transport: &mut Transport) {
        match msg {
            ServerMsg::Ready(ready) => {
                if let Some(missing) = missing_requirements(&ready) {
                    self.connection = Connection::Failed(missing.clone());
                    self.record(Record::error("ready.drift").msg(missing));
                    return;
                }
                self.connection = Connection::Ready(Box::new(ready));
                // Args are keyed by the method's parameter names, not by a shape of
                // our choosing: `session/list(_request)`.
                match transport.call("session", "list", serde_json::json!({ "_request": {} })) {
                    Ok(id) => self.session_list_call = Some(id),
                    Err(error) => self.push_log(error.to_string()),
                }
                // Control is host-wide: one stream serves every session's chips and jobs.
                match transport.open("session.control", serde_json::json!({})) {
                    Ok(id) => self.control_stream = Some(id),
                    Err(error) => self.push_log(error.to_string()),
                }
            }
            ServerMsg::Ok { id, v } => {
                if Some(id) == self.create_call {
                    self.create_call = None;
                    if let Some(text) = self.pending_prompt.take() {
                        match v.get("sessionId").and_then(Value::as_str) {
                            Some(session) => {
                                let session = session.to_string();
                                self.send_prompt(&session, text, transport);
                            }
                            None => self.fail_prompt(
                                text,
                                "the host created a session without naming it".to_string(),
                            ),
                        }
                    }
                    // Remember it so the refreshed list can select the new session rather
                    // than leaving the cursor on whatever was highlighted before.
                    self.pending_selection = v
                        .get("sessionId")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    if let Ok(call) =
                        transport.call("session", "list", serde_json::json!({ "_request": {} }))
                    {
                        self.session_list_call = Some(call);
                    }
                    return;
                }
                if Some(id) == self.page_call {
                    self.page_call = None;
                    let records = v
                        .get("records")
                        .cloned()
                        .and_then(|records| serde_json::from_value(records).ok())
                        .unwrap_or_default();
                    let has_more = v
                        .get("hasMore")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    match self.ledger.prepend_page(records, has_more) {
                        Apply::Applied | Apply::Duplicate => {}
                        other => self.push_log(format!("history page rejected: {other:?}")),
                    }
                    return;
                }
                if Some(id) == self.prompt_call {
                    self.prompt_call = None;
                    self.prompt_error = None;
                    self.record(Record::debug("prompt.accepted").msg("the host took the prompt"));
                    return;
                }
                if Some(id) == self.picker_call {
                    self.picker_call = None;
                    match serde_json::from_value::<Listing>(v) {
                        Ok(listing) => {
                            self.picker_error = None;
                            self.picker_climb = 0;
                            self.picker.get_or_insert_with(Browser::new).replace(listing);
                        }
                        Err(error) => {
                            self.picker_error =
                                Some(format!("the host's listing did not parse: {error}"));
                            self.push_log(format!("directoryPicker.list: {error}"));
                        }
                    }
                    return;
                }
                if Some(id) == self.presets_call {
                    self.presets_call = None;
                    self.preset_roster = serde_json::from_value(v).unwrap_or_default();
                    return;
                }
                if Some(id) == self.subagents_call {
                    self.subagents_call = None;
                    self.subagents = serde_json::from_value(v).unwrap_or_default();
                    return;
                }
                if Some(id) == self.skills_call {
                    self.skills_call = None;
                    // Skills join the `/` menu beside commands: both are `/` sources in
                    // the web client, and a reader does not care which registry served one.
                    self.candidates.extend(parse_skills(&v));
                    // Names are unique across the two registries in practice; dedupe by
                    // label so a shadowed entry cannot appear twice.
                    self.candidates.dedup_by(|a, b| a.label == b.label);
                    return;
                }
                if Some(id) == self.settings_call {
                    self.settings_call = None;
                    match serde_json::from_value::<Describe>(v) {
                        Ok(describe) => {
                            self.settings = Some(describe);
                            self.settings_ns = self.settings_ns.min(
                                describe_len(self.settings.as_ref()).saturating_sub(1),
                            );
                            self.settings_field = 0;
                        }
                        Err(error) => self.push_log(format!("settings.describe: {error}")),
                    }
                    // A preference changed elsewhere reaches this terminal here.
                    self.adopt_theme();
                    // A profile may name its own credential reference, so the reference
                    // set is only knowable once the document has arrived.
                    self.describe_credentials(transport);
                    return;
                }
                if Some(id) == self.settings_write {
                    self.settings_write = None;
                    self.settings_error = None;
                    // The reply is the updated namespace view; re-read the document so
                    // every namespace and its new revision stay consistent.
                    self.open_settings(transport);
                    return;
                }
                if Some(id) == self.providers_call {
                    self.providers_call = None;
                    self.providers = serde_json::from_value(v).unwrap_or_default();
                    self.describe_credentials(transport);
                    return;
                }
                if Some(id) == self.configurable_call {
                    self.configurable_call = None;
                    self.configurable = serde_json::from_value(v).unwrap_or_default();
                    self.describe_credentials(transport);
                    return;
                }
                if Some(id) == self.credentials_call {
                    self.credentials_call = None;
                    self.credential_error = None;
                    self.credentials = serde_json::from_value(v).unwrap_or_default();
                    return;
                }
                if Some(id) == self.catalog_call {
                    self.catalog_call = None;
                    self.catalog = Catalog::read(&v);
                    self.model_row = 0;
                    return;
                }
                if Some(id) == self.subagents_call {
                    self.subagents_call = None;
                    self.subagents = serde_json::from_value(v).unwrap_or_default();
                    return;
                }
                if Some(id) == self.skills_call {
                    self.skills_call = None;
                    // Skills join the `/` menu beside commands: both are `/` sources in
                    // the web client, and a reader does not care which registry served one.
                    self.candidates.extend(parse_skills(&v));
                    // Names are unique across the two registries in practice; dedupe by
                    // label so a shadowed entry cannot appear twice.
                    self.candidates.dedup_by(|a, b| a.label == b.label);
                    return;
                }
                if Some(id) == self.catalog_call {
                    self.catalog_call = None;
                    self.catalog = Catalog::read(&v);
                    self.model_row = 0;
                    return;
                }
                if Some(id) == self.inventory_call {
                    self.inventory_call = None;
                    match serde_json::from_value::<PluginSnapshot>(v) {
                        Ok(snapshot) => {
                            self.inventory.replace(snapshot);
                            self.inventory_row = 0;
                        }
                        Err(error) => self.push_log(format!("pluginInventory.list: {error}")),
                    }
                    return;
                }
                if Some(id) == self.candidate_call {
                    self.candidate_call = None;
                    // Two `/` sources answer independently, so replies merge into the
                    // bucket the current query owns rather than replacing each other.
                    self.candidates.extend(parse_candidates(&v));
                    self.candidate_index = 0;
                    return;
                }
                if Some(id) == self.settings_call {
                    self.settings_call = None;
                    match serde_json::from_value::<Describe>(v) {
                        Ok(describe) => {
                            self.settings = Some(describe);
                            self.settings_ns = 0;
                            self.settings_field = 0;
                        }
                        Err(error) => self.push_log(format!("settings.describe: {error}")),
                    }
                    // A preference changed elsewhere reaches this terminal here.
                    self.adopt_theme();
                    // A profile may name its own credential reference, so the reference
                    // set is only knowable once the document has arrived.
                    self.describe_credentials(transport);
                    return;
                }
                if Some(id) == self.settings_write {
                    self.settings_write = None;
                    self.settings_error = None;
                    // The reply is the updated namespace view; re-read so every namespace
                    // and its new revision stay consistent.
                    self.open_settings(transport);
                    return;
                }
                if let Some((pending, requested)) = self.workspace_create.clone() {
                    if pending == id {
                        self.workspace_create = None;
                        // The host canonicalises the path, so its answer wins over the
                        // string we sent; `created` says whether it was already known.
                        let registered = v
                            .get("workspace")
                            .and_then(|ws| ws.get("path"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or(requested);
                        let registered_id = v
                            .get("workspace")
                            .and_then(|ws| ws.get("workspaceId"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let created = v.get("created").and_then(Value::as_bool).unwrap_or(false);
                        self.record(
                            Record::info("workspace.registered")
                                .msg(if created {
                                    format!("registered {registered} as a new workspace")
                                } else {
                                    format!("{registered} was already a workspace")
                                })
                                .field("path", registered.clone())
                                .field("created", created),
                        );
                        self.adopt_workspace(registered, registered_id, transport);
                        return;
                    }
                }
                if Some(id) == self.session_list_call {
                    self.session_list_call = None;
                    self.sessions = parse_sessions(&v);
                    if let Some(wanted) = self.pending_selection.take() {
                        if let Some(index) =
                            self.sessions.iter().position(|row| row.id == wanted)
                        {
                            self.selected_session = index;
                        }
                    }
                    self.selected_session = self.selected_session.min(
                        self.sessions.len().saturating_sub(1),
                    );
                    self.rescope_to_workspace(transport);
                    self.follow_selected(transport);
                }
            }
            ServerMsg::Err { id, code, message, .. } => {
                if let Some((pending, requested)) = self.workspace_create.clone() {
                    if pending == id {
                        self.workspace_create = None;
                        // Registration is best effort: a harness that does not expose
                        // `workspace/create` must not leave the human unable to work. The
                        // path is still what they chose, so honour it and say what was
                        // lost — the choice will not survive this run.
                        self.record(
                            Record::warn("workspace.register.failed")
                                .msg(format!(
                                    "could not register {requested} with the host ({message}); \
                                     using it for this run only"
                                ))
                                .field("path", requested.clone())
                                .field("error", message.clone()),
                        );
                        self.adopt_workspace(requested, None, transport);
                        return;
                    }
                }
                if Some(id) == self.prompt_call {
                    self.prompt_call = None;
                    // The receipt the human is waiting for never came. Without this the
                    // composer simply emptied and nothing else happened.
                    self.fail_prompt(String::new(), message.clone());
                }
                if Some(id) == self.create_call {
                    // A workspace id the host no longer knows would otherwise leave every
                    // session creation failing with no way out. Forget it and try again
                    // by path, keeping any prompt that was waiting on the session.
                    if code == "workspace-not-found" && self.workspace_id.is_some() {
                        self.create_call = None;
                        self.workspace_id = None;
                        self.record(
                            Record::warn("session.create.retry")
                                .msg(format!("{message}; retrying with the directory")),
                        );
                        self.new_session(transport);
                        return;
                    }
                    if let Some(text) = self.pending_prompt.take() {
                        self.fail_prompt(text, format!("could not start a session: {message}"));
                    }
                }
                // Checked before the slot loop below, which clears `picker_call` and
                // would otherwise leave nothing to recognise this reply by.
                if Some(id) == self.picker_call {
                    // Refused by the host — a permission, a vanished directory, a root it
                    // will not browse above. Whatever it is, the dialog has to say it, or
                    // the key simply appears not to work.
                    self.picker_error = Some(message.clone());
                    self.record(
                        Record::warn("picker.refused")
                            .msg(format!("the host refused the listing: {message}")),
                    );
                }
                // Every in-flight call clears its slot; the ones with a visible surface
                // also say what the reader should conclude.
                for slot in [
                    &mut self.session_list_call,
                    &mut self.settings_call,
                    &mut self.providers_call,
                    &mut self.configurable_call,
                    &mut self.inventory_call,
                    &mut self.catalog_call,
                    &mut self.skills_call,
                    &mut self.subagents_call,
                    &mut self.presets_call,
                    &mut self.picker_call,
                    &mut self.page_call,
                    &mut self.create_call,
                ] {
                    if *slot == Some(id) {
                        *slot = None;
                    }
                }
                if Some(id) == self.candidate_call {
                    // A failed lookup closes the menu rather than freezing stale rows
                    // under a query they no longer answer.
                    self.candidate_call = None;
                    self.candidates.clear();
                }
                if Some(id) == self.settings_write {
                    self.settings_write = None;
                    // A stale `expectedRevision` is the expected conflict, not a crash:
                    // report it and re-read so the next edit starts from the current
                    // document rather than retrying against a revision that moved.
                    self.settings_error = Some(message.clone());
                    self.open_settings(transport);
                }
                if Some(id) == self.credentials_call {
                    self.credentials_call = None;
                    // Credential state enriches the Models page; neither a business
                    // rejection nor a transport failure should empty it.
                    self.credential_error = Some(message.clone());
                }
            }
            ServerMsg::Event { event, .. } => {
                if event.starts_with("api-session/") {
                    // Session membership and status changed; the list is the cheap
                    // authority and re-reading it does not activate an agent.
                    if let Ok(id) =
                        transport.call("session", "list", serde_json::json!({ "_request": {} }))
                    {
                        self.session_list_call = Some(id);
                    }
                }
            }
            ServerMsg::Ask { id, event, args, .. } => {
                // `ask_user_question` has its own surface: the composer takeover. Only
                // approvals use the generic modal.
                if event == "user-questions/request" {
                    let parsed = args
                        .first()
                        .cloned()
                        .and_then(|value| serde_json::from_value::<QuestionRequest>(value).ok());
                    match parsed {
                        Some(request) if !request.questions.is_empty() => {
                            self.questions = Some(PendingQuestions {
                                id,
                                request,
                                draft: AnswerDraft::new(),
                                question_index: 0,
                                option_index: 0,
                            });
                            return;
                        }
                        // A shape this build cannot render must be delegated, not
                        // answered: the generic modal path enforces that.
                        _ => {}
                    }
                }
                self.asks.push_back(PendingAsk { id, event, args });
            }
            ServerMsg::Item { id, gen, v } => {
                if Some(id) == self.control_stream {
                    match serde_json::from_value::<ControlFrame>(v) {
                        Ok(frame) => {
                            if !self.control.apply(gen, frame) {
                                self.push_log(
                                    "dropped a control frame without its baseline".to_string(),
                                );
                            }
                        }
                        Err(error) => self.push_log(format!("control frame: {error}")),
                    }
                    return;
                }
                if Some(id) == self.control_stream {
                    match serde_json::from_value::<ControlFrame>(v) {
                        Ok(frame) => {
                            if !self.control.apply(gen, frame) {
                                self.push_log(
                                    "dropped a control frame without its baseline".to_string(),
                                );
                            }
                        }
                        Err(error) => self.push_log(format!("control frame: {error}")),
                    }
                    return;
                }
                if Some(id) == self.workspace_stream {
                    match serde_json::from_value::<WorkspaceFrame>(v) {
                        Ok(frame) => {
                            if self.workspaces.apply(gen, frame) {
                                // The rows are what scopes the sidebar, so a workspace
                                // chosen before its row arrived only takes effect here.
                                self.rescope_to_workspace(transport);
                            } else {
                                self.push_log(
                                    "dropped a workspace increment without its baseline"
                                        .to_string(),
                                );
                            }
                        }
                        Err(error) => self.push_log(format!("workspace frame: {error}")),
                    }
                    return;
                }
                if Some(id) != self.follow_stream {
                    return;
                }
                let Ok(item) = serde_json::from_value::<JournalItem>(v) else {
                    self.push_log("journal item did not match the record contract".to_string());
                    return;
                };
                match self.ledger.apply(gen, &item) {
                    Apply::Applied | Apply::Duplicate => {}
                    Apply::Gap { expected, got } => {
                        // The stream is contiguous by contract, so a gap means a dropped
                        // or reordered batch: repair from a tail page rather than
                        // rendering a transcript with a hole in it.
                        self.push_log(format!(
                            "history gap at seq {expected} (received {got}); repairing"
                        ));
                        self.repair_history(transport);
                    }
                    Apply::PartialOverlap => {
                        self.push_log("history batch partially overlapped; repairing".to_string());
                        self.repair_history(transport);
                    }
                    Apply::Malformed => {
                        self.push_log("discarded a non-contiguous history batch".to_string());
                    }
                }
            }
            ServerMsg::End { id, .. } => {
                if Some(id) == self.follow_stream {
                    self.follow_stream = None;
                }
                if Some(id) == self.workspace_stream {
                    self.workspace_stream = None;
                }
                if Some(id) == self.control_stream {
                    self.control_stream = None;
                }
            }
            // A terminal stream failure needs no app-side state change: the transport
            // records it with the stream's name, duration and item count, and the pane
            // that was reading it already shows what it has.
            ServerMsg::StreamErr { .. } => {}
            ServerMsg::Bye => {
                self.should_quit = true;
            }
        }
    }

    /// Show the settings form, reading the document if it is not loaded.
    pub fn open_settings(&mut self, transport: &mut Transport) {
        self.view = View::Settings;
        self.settings_section = SettingsSection::Namespaces;
        if self.settings_call.is_some() {
            return;
        }
        match transport.call("settings", "describe", serde_json::json!({})) {
            Ok(id) => self.settings_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// The selected session's id, if any.
    /// Open the highlighted session and hand the keyboard to the conversation.
    ///
    /// Moving the cursor already binds the transcript, so this is about *continuing*: it
    /// puts focus where typing goes, which is what opening a previous session is for.
    pub fn open_selected_session(&mut self, transport: &mut Transport) {
        let Some(session) = self.sessions.get(self.selected_session).cloned() else { return };
        let owner = self.workspace_of(&session);
        self.record(
            Record::info("session.open")
                .msg(format!("opened {}", session.title))
                .field("sessionId", session.id.clone())
                .field(
                    "workspace",
                    owner
                        .as_ref()
                        .map(|row| Value::from(row.path.clone()))
                        .unwrap_or(Value::Null),
                ),
        );
        // Opening a session moves the whole context to it: the status bar, the sidebar
        // header and the scope all name the workspace it belongs to. Leaving them on the
        // previous workspace would describe somewhere the open session does not live.
        if let Some(row) = owner {
            if self.workspace.as_deref() != Some(row.path.as_str()) {
                let id = row.workspace_id.clone();
                self.workspace = Some(row.path.clone());
                self.workspace_id = Some(id);
                self.record(
                    Record::info("workspace.followed")
                        .msg(format!("moved to {} with the session", row.path))
                        .field("path", row.path.clone()),
                );
            }
        }
        self.follow_selected(transport);
        self.focus = Pane::Conversation;
    }

    /// Bind the transcript to a session without a stream, for render tests.
    #[doc(hidden)]
    pub fn set_followed_session_for_test(&mut self, id: Option<String>) {
        self.followed_session = id;
    }

    /// Force the workspace id, for tests that need to play a stale one.
    #[doc(hidden)]
    pub fn set_workspace_id_for_test(&mut self, id: Option<String>) {
        self.workspace_id = id;
    }

    /// The session the transcript is currently bound to, if any.
    pub fn followed_session(&self) -> Option<&str> {
        self.followed_session.as_deref()
    }

    pub fn active_session_id(&self) -> Option<&str> {
        // Scoped: with the cursor outside the workspace there is no active session, and
        // saying otherwise would send this workspace's prompt to another one's session.
        if !self.visible_sessions().contains(&self.selected_session) {
            return None;
        }
        self.sessions.get(self.selected_session).map(|s| s.id.as_str())
    }

    /// The projection map for the selected session.
    fn active_projections(&self) -> serde_json::Value {
        self.active_session_id()
            .map(|id| self.control.projections(id))
            .unwrap_or(serde_json::Value::Null)
    }

    /// The plan chip's state.
    pub fn plan_chip(&self) -> PlanChip {
        let projections = self.active_projections();
        PlanChip::read(Some(&projections))
    }

    /// The current goal, if the session has one.
    pub fn goal(&self) -> Option<Goal> {
        let projections = self.active_projections();
        Goal::read(Some(&projections))
    }

    /// Live background jobs for the selected session.
    pub fn live_jobs(&self) -> usize {
        self.active_session_id()
            .map(|id| self.control.live_jobs(id))
            .unwrap_or(0)
    }

    /// Open the model picker, reading the catalog once.
    pub fn open_model_picker(&mut self, transport: &mut Transport) {
        self.model_picker = Some(String::new());
        self.model_row = 0;
        if !self.catalog.choices.is_empty() || self.catalog_call.is_some() {
            return;
        }
        match transport.call("session", "modelCatalog", serde_json::json!({})) {
            Ok(id) => self.catalog_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    pub fn close_model_picker(&mut self) {
        self.model_picker = None;
    }

    /// Choices matching the picker's query.
    pub fn model_choices(&self) -> Vec<&crate::chips::ModelChoice> {
        let query = self.model_picker.as_deref().unwrap_or_default();
        self.catalog.filter(query)
    }

    pub fn select_next_model(&mut self) {
        let count = self.model_choices().len();
        if count > 0 {
            self.model_row = (self.model_row + 1).min(count - 1);
        }
    }

    pub fn select_prev_model(&mut self) {
        self.model_row = self.model_row.saturating_sub(1);
    }

    /// Install the highlighted model on the current session.
    pub fn pick_model(&mut self, transport: &mut Transport) {
        let picked = {
            let choices = self.model_choices();
            choices.get(self.model_row).map(|choice| {
                (choice.provider.clone(), choice.model.clone())
            })
        };
        let (Some((provider, model)), Some(session)) =
            (picked, self.active_session_id().map(str::to_string))
        else {
            return;
        };
        let args = serde_json::json!({
            "request": { "sessionId": session, "provider": provider, "model": model }
        });
        if let Err(error) = transport.call("session", "selectModel", args) {
            self.push_log(error.to_string());
        }
        self.model_picker = None;
    }

    /// Edit the model picker's query.
    pub fn search_models(&mut self, edit: SearchEdit) {
        let Some(query) = self.model_picker.as_mut() else { return };
        match edit {
            SearchEdit::Push(ch) => query.push(ch),
            SearchEdit::Backspace => {
                query.pop();
            }
            SearchEdit::Clear => query.clear(),
        }
        let count = self.model_choices().len();
        self.model_row = self.model_row.min(count.saturating_sub(1));
    }

    /// The session's permission select, or `None` when no permission service is composed.
    pub fn permissions(&self) -> Option<PermissionSelect> {
        let projections = self.active_projections();
        PermissionSelect::read(Some(&projections))
    }

    /// Open the workspace directory browser at `path`, or at the default root.
    ///
    /// A listing already in flight is **superseded**, not a reason to ignore the key.
    /// Dropping it made a second press do nothing at all, which is indistinguishable from
    /// a directory that cannot be entered.
    pub fn browse_directory(&mut self, path: Option<&str>, transport: &mut Transport) {
        if let Some(previous) = self.picker_call.take() {
            let _ = transport.send(ClientMsg::Cancel { id: previous });
        }
        self.picker_error = None;
        // The host owns path resolution; the client sends back only paths it was given.
        let args = match path {
            Some(path) => serde_json::json!({ "path": path }),
            None => serde_json::json!({}),
        };
        self.record(
            Record::debug("picker.list")
                .msg(format!("listing {}", path.unwrap_or("(default root)")))
                .field("path", path.map(Value::from).unwrap_or(Value::Null)),
        );
        match transport.call("directoryPicker", "list", args) {
            Ok(id) => self.picker_call = Some(id),
            Err(error) => {
                self.picker_error = Some(error.to_string());
                self.push_log(error.to_string());
            }
        }
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
        self.picker_call = None;
        self.picker_error = None;
        self.picker_climb = 0;
    }

    /// Dismiss the picker without choosing, abandoning any creation waiting on it.
    ///
    /// Leaving the flag set would make the *next* directory the human browses to silently
    /// create a session they did not ask for.
    pub fn cancel_picker(&mut self) {
        if std::mem::take(&mut self.pending_new_session) {
            self.record(
                Record::info("session.create.cancelled")
                    .msg("no workspace chosen; the session was not created"),
            );
        }
        self.close_picker();
    }

    /// Enter the highlighted directory.
    pub fn enter_directory(&mut self, transport: &mut Transport) {
        let path = self
            .picker
            .as_ref()
            .and_then(Browser::selected_path)
            .map(str::to_string);
        if let Some(path) = path {
            self.picker_climb = 0;
            self.browse_directory(Some(&path), transport);
        }
    }

    /// Go up one level, using the crumb chain rather than trimming the path.
    ///
    /// The crumb chain is the host's own ancestry for the listed directory. When there is
    /// no crumb above the current one there is nowhere to go, and saying so beats a key
    /// that silently does nothing.
    pub fn leave_directory(&mut self, transport: &mut Transport) {
        let climb = self.picker_climb;
        let path = self
            .picker
            .as_ref()
            .and_then(|picker| picker.ancestor(climb))
            .map(str::to_string);
        match path {
            Some(path) => {
                // Each press rises one more level even while the previous listing is
                // still in flight, so holding `←` climbs instead of re-requesting the
                // same parent.
                self.picker_climb += 1;
                self.browse_directory(Some(&path), transport);
            }
            None => {
                let at = self
                    .picker
                    .as_ref()
                    .map(|picker| picker.breadcrumb())
                    .unwrap_or_default();
                self.picker_error =
                    Some(format!("{at} is the top of what the host will browse"));
                self.record(
                    Record::info("picker.noParent")
                        .msg("no crumb above the listed directory")
                        .field("path", at),
                );
            }
        }
    }

    /// The host's image-intake limits, if the capability is composed.
    pub fn image_limits(&self) -> Option<Limits> {
        let projections = self.active_projections();
        Limits::read(Some(&projections))
    }

    /// Stage an image for the next message, checking it against the host's limits first.
    ///
    /// The host enforces these anyway; checking here turns a rejected turn into an
    /// immediate message naming the file the user just chose.
    pub fn attach_image(&mut self, draft: Draft) {
        let Some(limits) = self.image_limits() else {
            self.attachment_error =
                Some("this deployment does not accept image attachments".to_string());
            return;
        };
        match attachment::admit(&self.drafts, &draft, &limits) {
            Ok(()) => {
                self.attachment_error = None;
                self.drafts.push(draft);
            }
            Err(error) => self.attachment_error = Some(error),
        }
    }

    pub fn clear_drafts(&mut self) {
        self.drafts.clear();
        self.attachment_error = None;
    }

    /// Read the agent-preset roster.
    pub fn load_presets(&mut self, transport: &mut Transport) {
        if self.presets_call.is_some() || !self.preset_roster.presets.is_empty() {
            return;
        }
        match transport.call("agentPresets", "list", serde_json::json!({})) {
            Ok(id) => self.presets_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Workflow runs folded from the current conversation.
    pub fn workflow_runs(&self) -> Vec<crate::workflow::Run> {
        crate::workflow::runs(&self.ledger)
    }

    /// Files the current conversation produced.
    pub fn produced_files(&self) -> Vec<String> {
        crate::deliverables::produced(&self.ledger)
    }

    /// Read the direct-child catalog for the selected session.
    pub fn load_subagents(&mut self, transport: &mut Transport) {
        let Some(session) = self.active_session_id().map(str::to_string) else {
            return;
        };
        if self.subagents_call.is_some() {
            return;
        }
        // Not every deployment mounts the subagent Remote; a missing namespace answers
        // `not found`, which the error path reports without emptying anything else.
        match transport.call("subagent", "list", serde_json::json!({ "parentSessionId": session })) {
            Ok(id) => self.subagents_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Show the Models page, reading the provider directory.
    pub fn open_models(&mut self, transport: &mut Transport) {
        self.view = View::Settings;
        self.settings_section = SettingsSection::Models;
        if self.settings.is_none() && self.settings_call.is_none() {
            match transport.call("settings", "describe", serde_json::json!({})) {
                Ok(id) => self.settings_call = Some(id),
                Err(error) => self.push_log(error.to_string()),
            }
        }
        if self.providers.is_empty() && self.providers_call.is_none() {
            match transport.call("llm", "listProviders", serde_json::json!({})) {
                Ok(id) => self.providers_call = Some(id),
                Err(error) => self.push_log(error.to_string()),
            }
        }
        if self.configurable.is_empty() && self.configurable_call.is_none() {
            match transport.call("llm", "listConfigurableProviders", serde_json::json!({})) {
                Ok(id) => self.configurable_call = Some(id),
                Err(error) => self.push_log(error.to_string()),
            }
        }
    }

    /// Show the read-only Loader inventory.
    pub fn open_plugins(&mut self, transport: &mut Transport) {
        self.view = View::Settings;
        self.settings_section = SettingsSection::Plugins;
        if self.inventory_call.is_some() {
            return;
        }
        // A point-in-time projection: re-read on each visit rather than caching a tree
        // that changes as plugins load and unload.
        match transport.call("pluginInventory", "list", serde_json::json!({})) {
            Ok(id) => self.inventory_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Show the General page.
    pub fn open_general(&mut self, transport: &mut Transport) {
        self.view = View::Settings;
        self.settings_section = SettingsSection::General;
        if self.settings.is_none() && self.settings_call.is_none() {
            match transport.call("settings", "describe", serde_json::json!({})) {
                Ok(id) => self.settings_call = Some(id),
                Err(error) => self.push_log(error.to_string()),
            }
        }
    }

    pub fn is_general_page(&self) -> bool {
        self.view == View::Settings && self.settings_section == SettingsSection::General
    }

    /// Adopt the theme preference stored in the `ui-theme` namespace.
    ///
    /// Called whenever the settings document arrives, so a preference changed elsewhere —
    /// in the browser, say — reaches this terminal on its next read.
    pub fn adopt_theme(&mut self) {
        let stored = self
            .settings
            .as_ref()
            .and_then(|d| d.namespaces.iter().find(|ns| ns.ns == "ui-theme"))
            .and_then(|ns| ns.value.get("preference"))
            .and_then(|value| value.as_str())
            .and_then(Preference::parse);
        let Some(preference) = stored else { return };
        self.theme_preference = preference;
        let resolved = theme::resolve(preference, self.colorfgbg.as_deref());
        self.theme_reason = resolved.reason;
        self.theme = Theme::new(resolved.mode);
    }

    /// Cycle the theme preference and persist it, applying it immediately.
    pub fn cycle_theme(&mut self, transport: &mut Transport) {
        let next = match self.theme_preference {
            Preference::Light => Preference::Dark,
            Preference::Dark => Preference::System,
            Preference::System => Preference::Light,
        };
        self.theme_preference = next;
        let resolved = theme::resolve(next, self.colorfgbg.as_deref());
        self.theme_reason = resolved.reason;
        // Apply locally first: the redraw should not wait on a round trip.
        self.theme = Theme::new(resolved.mode);

        let Some(view) = self
            .settings
            .as_ref()
            .and_then(|d| d.namespaces.iter().find(|ns| ns.ns == "ui-theme"))
            .cloned()
        else {
            return;
        };
        let args = serde_json::json!({
            "ns": "ui-theme",
            "ops": [{ "op": "set", "path": ["preference"], "value": next.as_str() }],
            "expectedRevision": view.revision,
        });
        match transport.call("settings", "mutate", args) {
            Ok(id) => self.settings_write = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    pub fn is_plugins_page(&self) -> bool {
        self.view == View::Settings && self.settings_section == SettingsSection::Plugins
    }

    pub fn select_next_inventory_row(&mut self) {
        let count = self.inventory.rows().len();
        if count > 0 {
            self.inventory_row = (self.inventory_row + 1).min(count - 1);
        }
    }

    pub fn select_prev_inventory_row(&mut self) {
        self.inventory_row = self.inventory_row.saturating_sub(1);
    }

    /// Edit the inventory search, keeping the selection inside the filtered rows.
    pub fn search_inventory(&mut self, edit: SearchEdit) {
        match edit {
            SearchEdit::Push(ch) => self.inventory.query.push(ch),
            SearchEdit::Backspace => {
                self.inventory.query.pop();
            }
            SearchEdit::Clear => self.inventory.query.clear(),
        }
        // A narrowed list must not leave the cursor pointing past its end.
        let count = self.inventory.rows().len();
        self.inventory_row = self.inventory_row.min(count.saturating_sub(1));
    }

    /// Read credential state for every reference the rows resolve through.
    ///
    /// Needs both the configurable directory and the settings document, since a profile
    /// may name its own reference instead of using the derived one.
    fn describe_credentials(&mut self, transport: &mut Transport) {
        if self.configurable.is_empty() || self.credentials_call.is_some() {
            return;
        }
        let namespaces = self
            .settings
            .as_ref()
            .map(|d| d.namespaces.as_slice())
            .unwrap_or(&[]);
        let refs = models::credential_refs(&self.configurable, namespaces);
        if refs.is_empty() {
            return;
        }
        match transport.call("credentials", "describe", serde_json::json!({ "refs": refs })) {
            Ok(id) => self.credentials_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// The joined Models rows.
    pub fn model_rows(&self) -> Vec<ProviderRow> {
        let namespaces = self
            .settings
            .as_ref()
            .map(|d| d.namespaces.as_slice())
            .unwrap_or(&[]);
        models::rows(&self.providers, &self.configurable, namespaces, &self.credentials)
    }

    /// Switch between the settings pages.
    pub fn next_settings_section(&mut self, transport: &mut Transport) {
        match self.settings_section {
            SettingsSection::General => self.settings_section = SettingsSection::Namespaces,
            SettingsSection::Namespaces => self.open_models(transport),
            SettingsSection::Models => self.open_plugins(transport),
            SettingsSection::Plugins => self.open_general(transport),
        }
    }

    /// Whether the Models page is the visible surface.
    pub fn is_models_page(&self) -> bool {
        self.view == View::Settings && self.settings_section == SettingsSection::Models
    }

    pub fn select_next_model_row(&mut self) {
        let count = self.model_rows().len();
        if count > 0 {
            self.models_row = (self.models_row + 1).min(count - 1);
        }
    }

    pub fn select_prev_model_row(&mut self) {
        self.models_row = self.models_row.saturating_sub(1);
    }

    /// Show the workspace browser, opening its stream once.
    pub fn open_workspace(&mut self, transport: &mut Transport) {
        self.view = View::Workspace;
        if self.workspace_stream.is_some() {
            return;
        }
        match transport.open("workspace.follow", serde_json::json!({})) {
            Ok(id) => self.workspace_stream = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    pub fn show_conversation(&mut self) {
        self.view = View::Conversation;
    }

    // ── the log surface ──────────────────────────────────────────────────────

    /// Open the log pane, pinned to the newest record.
    pub fn open_logs(&mut self) {
        self.view = View::Logs;
        self.log_scroll.to_bottom();
        self.record(
            Record::debug("ui.logs.open")
                .field("held", self.log.len())
                .field("file", path_field()),
        );
    }

    /// The records the pane is currently showing, oldest first.
    pub fn visible_logs(&self) -> Vec<&Record> {
        self.log
            .iter()
            .filter(|record| record.level <= self.log_min && record.contains(&self.log_filter))
            .collect()
    }

    /// Narrow or widen the pane's threshold. Wraps, so one key reaches every setting.
    pub fn cycle_log_level(&mut self) {
        let next = self.log_min.cycle();
        // Offering a level the file never recorded would show an empty pane and read as
        // a bug; wrap back to the strictest instead.
        self.log_min = if next > logging::level() { Level::Error } else { next };
        self.log_scroll.to_bottom();
    }

    pub fn edit_log_filter(&mut self, edit: SearchEdit) {
        match edit {
            SearchEdit::Push(c) => self.log_filter.push(c),
            SearchEdit::Backspace => {
                self.log_filter.pop();
            }
            SearchEdit::Clear => self.log_filter.clear(),
        }
        // A changed filter changes what is scrollable, so the old offset means nothing.
        self.log_scroll.to_bottom();
    }

    pub fn scroll_logs(&mut self, delta: ScrollDelta) {
        match delta {
            ScrollDelta::LineUp => self.log_scroll.scroll_up(1),
            ScrollDelta::LineDown => self.log_scroll.scroll_down(1),
            ScrollDelta::PageUp => self.log_scroll.page_up(),
            ScrollDelta::PageDown => self.log_scroll.page_down(),
            ScrollDelta::Top => self.log_scroll.to_top(),
            ScrollDelta::Bottom => self.log_scroll.to_bottom(),
        }
    }

    /// The log file for this run, for the pane's header.
    pub fn log_path(&self) -> Option<String> {
        logging::path().map(|path| path.display().to_string())
    }

    /// The same path with `$HOME` abbreviated, which is the difference between fitting in
    /// the header and being clipped. `^y` copies the unabbreviated one.
    pub fn log_path_short(&self) -> Option<String> {
        let path = self.log_path()?;
        let Ok(home) = std::env::var("HOME") else { return Some(path) };
        if home.is_empty() {
            return Some(path);
        }
        Some(match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path,
        })
    }

    /// The namespace currently selected in the settings form.
    pub fn settings_namespace(&self) -> Option<&crate::settings::NamespaceView> {
        self.settings.as_ref()?.namespaces.get(self.settings_ns)
    }

    /// Fields of the selected namespace.
    pub fn settings_fields(&self) -> Vec<Field> {
        self.settings_namespace().map(settings::fields).unwrap_or_default()
    }

    pub fn select_next_namespace(&mut self) {
        let count = self.settings.as_ref().map(|d| d.namespaces.len()).unwrap_or(0);
        if count > 0 {
            self.settings_ns = (self.settings_ns + 1) % count;
            self.settings_field = 0;
        }
    }

    pub fn select_prev_namespace(&mut self) {
        let count = self.settings.as_ref().map(|d| d.namespaces.len()).unwrap_or(0);
        if count > 0 {
            self.settings_ns = (self.settings_ns + count - 1) % count;
            self.settings_field = 0;
        }
    }

    pub fn select_next_field(&mut self) {
        let count = self.settings_fields().len();
        if count > 0 {
            self.settings_field = (self.settings_field + 1).min(count - 1);
        }
    }

    pub fn select_prev_field(&mut self) {
        self.settings_field = self.settings_field.saturating_sub(1);
    }

    /// Write one field edit, carrying the view's revision.
    ///
    /// A secret never travels through `settings.mutate`: its value goes to the credentials
    /// namespace, and the settings document only records that a slot is filled.
    pub fn write_setting(&mut self, input: &str, transport: &mut Transport) {
        let Some(view) = self.settings_namespace().cloned() else {
            return;
        };
        let fields = settings::fields(&view);
        let Some(field) = fields.get(self.settings_field) else {
            return;
        };
        if field.disabled {
            self.settings_error = Some(format!("{} is not editable", field.label));
            return;
        }
        if matches!(field.editor, crate::settings::Editor::Secret { .. }) {
            self.settings_error =
                Some("secrets are set through credentials, not the settings document".to_string());
            return;
        }
        let op = match settings::edit_op(field, input) {
            Ok(op) => op,
            Err(error) => {
                self.settings_error = Some(error);
                return;
            }
        };
        self.settings_error = None;
        let args = serde_json::json!({
            "ns": view.ns,
            "ops": [op],
            "expectedRevision": view.revision,
        });
        match transport.call("settings", "mutate", args) {
            Ok(id) => self.settings_write = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Re-read candidates whenever the trigger under the caret changes.
    ///
    /// Called after every composer edit. A trigger that has not changed issues no call, so
    /// arrow keys and cursor moves inside the same query stay silent.
    pub fn refresh_candidates(&mut self, transport: &mut Transport) {
        let Some(trigger) = self.composer.active_trigger() else {
            self.candidates.clear();
            self.candidate_query = None;
            self.candidate_call = None;
            return;
        };
        let key = (trigger.kind, trigger.query.clone());
        if self.candidate_query.as_ref() == Some(&key) {
            return;
        }
        // The bucket belongs to one query; a new query starts empty so a slow reply for
        // the previous one cannot land among the new rows.
        self.candidates.clear();
        self.candidate_index = 0;
        self.candidate_query = Some(key);

        let session = self.sessions.get(self.selected_session).map(|s| s.id.clone());
        let result = match trigger.kind {
            TriggerKind::Slash => {
                // Skills are a second `/` source; ask for both and merge the replies.
                match transport.call(
                    "skills",
                    "list",
                    serde_json::json!({ "request": { "sessionId": session } }),
                ) {
                    Ok(id) => self.skills_call = Some(id),
                    Err(error) => self.push_log(error.to_string()),
                }
                // `commands/list` addresses its agent by id, not by a request object.
                transport.call("commands", "list", serde_json::json!({ "agentId": session }))
            }
            TriggerKind::At => transport.call(
                "fileReferences",
                "list",
                serde_json::json!({ "agent": session, "query": trigger.query }),
            ),
        };
        match result {
            Ok(id) => self.candidate_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Candidates matching the current query, for menus the host returns unfiltered.
    pub fn visible_candidates(&self) -> Vec<&Candidate> {
        let query = self
            .composer
            .active_trigger()
            .map(|trigger| trigger.query.to_lowercase())
            .unwrap_or_default();
        self.candidates
            .iter()
            .filter(|candidate| {
                query.is_empty() || candidate.label.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn select_prev_candidate(&mut self) {
        self.candidate_index = self.candidate_index.saturating_sub(1);
    }

    pub fn select_next_candidate(&mut self) {
        let count = self.visible_candidates().len();
        if count > 0 {
            self.candidate_index = (self.candidate_index + 1).min(count - 1);
        }
    }

    /// Insert the highlighted candidate into the composer.
    pub fn pick_candidate(&mut self, transport: &mut Transport) {
        let insert = {
            let visible = self.visible_candidates();
            let Some(candidate) = visible.get(self.candidate_index) else {
                return;
            };
            candidate.insert.clone()
        };
        self.composer.apply_pick(&insert);
        self.candidates.clear();
        self.candidate_query = None;
        self.refresh_candidates(transport);
    }

    /// Bind the follow stream to the selected session, replacing any previous one.
    pub fn follow_selected(&mut self, transport: &mut Transport) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        if self.followed_session.as_deref() == Some(session.id.as_str())
            && self.follow_stream.is_some()
        {
            return;
        }
        if let Some(previous) = self.follow_stream.take() {
            let _ = transport.send(ClientMsg::Close { id: previous });
        }
        let id = session.id.clone();
        // The ledger belongs to one session; carrying rows across would splice two
        // transcripts together, and a scroll offset or search into the old one is
        // meaningless against the new.
        self.ledger = Ledger::new();
        self.scroll = Viewport::new();
        self.close_search();
        // A durable address is a tagged union; `kind` selects the session variant.
        let request = serde_json::json!({
            "request": { "address": { "kind": "session", "sessionId": id } }
        });
        match transport.open("session.follow", request) {
            Ok(stream) => {
                self.follow_stream = Some(stream);
                self.followed_session = Some(id);
            }
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Request the page of history immediately older than what is held.
    ///
    /// `throughSeq` quotes the follow opening frame's cut verbatim, so events appended
    /// since then cannot shift the page boundaries under the reader.
    pub fn load_older(&mut self, transport: &mut Transport) {
        if self.page_call.is_some() || !self.ledger.has_more() {
            return;
        }
        let (Some(session), Some(through_seq)) = (
            self.followed_session.clone(),
            self.ledger.cursor(),
        ) else {
            return;
        };
        let mut request = serde_json::json!({
            "address": { "kind": "session", "sessionId": session },
            "throughSeq": through_seq,
        });
        if let Some((oldest, _)) = self.ledger.span() {
            request["beforeSeq"] = serde_json::json!(oldest);
        }
        let args = serde_json::json!({ "request": request });
        match transport.call("session", "page", args) {
            Ok(id) => self.page_call = Some(id),
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Scroll the conversation, requesting older history on reaching the top.
    pub fn scroll_conversation(&mut self, delta: ScrollDelta, transport: &mut Transport) {
        match delta {
            ScrollDelta::LineUp => self.scroll.scroll_up(1),
            ScrollDelta::LineDown => self.scroll.scroll_down(1),
            ScrollDelta::PageUp => self.scroll.page_up(),
            ScrollDelta::PageDown => self.scroll.page_down(),
            ScrollDelta::Top => self.scroll.to_top(),
            ScrollDelta::Bottom => self.scroll.to_bottom(),
        }
        // Backfill when the oldest held line comes into view, so scrolling continues
        // rather than stopping at whatever the opening window happened to contain.
        if self.scroll.is_at_top() {
            self.load_older(transport);
        }
    }

    /// Re-measure the conversation for scrolling and search.
    ///
    /// Called before each draw: the viewport needs the current line count and visible
    /// height, and search matches are line indices into the same list.
    pub fn measure(&mut self, terminal_height: u16) {
        self.rendered = crate::ui::conversation_text(self);
        self.scroll
            .layout(self.rendered.len(), conversation_height(terminal_height));
        // The log pane is a tail: without a layout it would render from the oldest
        // record, which is the opposite of what someone opening a log wants to see.
        let shown = self.visible_logs().len();
        self.log_scroll.layout(shown, log_height(terminal_height));
        // Paging needs the real height of the popup, which only the frame knows.
        let truncated = self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.truncation_note().is_some());
        self.picker_page = picker_page(terminal_height, truncated);
        if let Some(query) = self.search.clone() {
            let hits = crate::viewport::matches(&self.rendered, &query);
            // Keep the current match selected across a redraw when it still matches.
            let current = self.current_match();
            self.search_hits = hits;
            self.search_index = current
                .and_then(|line| self.search_hits.iter().position(|hit| *hit == line))
                .unwrap_or(0);
        }
    }

    /// Escape sequences drawing every staged image, and the placeholder text for those
    /// that cannot be drawn.
    ///
    /// Called after the frame is drawn, so the terminal writes the image over cells the
    /// buffer has already painted.
    pub fn image_placements(&self, width: u16, height: u16) -> (Vec<Placement>, Vec<String>) {
        let mut placements = Vec::new();
        let mut unsupported = Vec::new();
        if self.drafts.is_empty() {
            return (placements, unsupported);
        }
        let row = image_row(width, height);
        // Side by side across the rail, each in its own cell box.
        let each = (row.cols / self.drafts.len().max(1) as u16).max(1);
        for (index, draft) in self.drafts.iter().enumerate() {
            let rect = CellRect {
                x: row.x + each * index as u16,
                y: row.y,
                cols: each.saturating_sub(1).max(1),
                rows: row.rows,
            };
            match image::placement(self.graphics, &draft.media_type, &draft.data, rect) {
                Ok(placement) => placements.push(placement),
                Err(why) => unsupported.push(format!(
                    "{} — {}",
                    crate::attachment::placeholder(draft),
                    image::reason(why)
                )),
            }
        }
        (placements, unsupported)
    }

    /// Send the composer's text to the selected session.
    ///
    /// `requestId` is client-minted and persists on the accepted message, which is what
    /// lets a local submission echo be retired when its durable event arrives.
    pub fn submit_prompt(&mut self, transport: &mut Transport) {
        let text = self.composer.take();
        if text.trim().is_empty() {
            return;
        }
        let Some(session) = self.active_session_id().map(str::to_string) else {
            // A fresh workspace has no session yet. Making the human create one before
            // they may type is a step with no decision in it: create it and send the
            // prompt once it exists.
            if self.workspace.is_some() {
                // A creation already in flight will carry this too. Starting another
                // would give every message its own session, which is what the log of a
                // workspace the host never accounted actually showed.
                if self.create_call.is_some() {
                    let queued = match self.pending_prompt.take() {
                        Some(waiting) => format!("{waiting}\n{text}"),
                        None => text,
                    };
                    self.pending_prompt = Some(queued);
                    self.record(
                        Record::info("prompt.queued")
                            .msg("a session is already being created; queued behind it"),
                    );
                    return;
                }
                self.record(
                    Record::info("prompt.deferred")
                        .msg("no session in this workspace yet; creating one to carry the prompt"),
                );
                self.pending_prompt = Some(text);
                self.new_session(transport);
            } else {
                self.push_log("no session selected".to_string());
            }
            return;
        };
        self.send_prompt(&session, text, transport);
    }

    /// Put one prompt on the wire, tracking it so its rejection can be shown.
    fn send_prompt(&mut self, session: &str, text: String, transport: &mut Transport) {
        let args = serde_json::json!({
            "request": {
                "requestId": uuid::Uuid::new_v4().to_string(),
                "sessionId": session,
                // `steer` interrupts a running turn; a composer submit queues.
                "mode": "queue",
                "content": [{ "type": "text", "text": text.clone() }],
            }
        });
        match transport.call("session", "prompt", args) {
            Ok(id) => {
                self.prompt_call = Some(id);
                self.prompt_error = None;
                self.record(
                    Record::info("prompt.sent")
                        .msg(format!("sent {} chars", text.chars().count()))
                        .field("sessionId", session.to_string()),
                );
            }
            Err(error) => self.fail_prompt(text, error.to_string()),
        }
    }

    /// A prompt that did not go out: say so, and give the text back.
    ///
    /// `submit_prompt` empties the composer before it knows whether the send will work,
    /// so a failure here would otherwise lose what the human typed with nothing on screen
    /// to explain where it went.
    fn fail_prompt(&mut self, text: String, reason: String) {
        self.record(
            Record::error("prompt.failed")
                .msg(reason.clone())
                .field("chars", text.chars().count()),
        );
        self.prompt_error = Some(reason);
        if self.composer.is_empty() {
            self.composer.set(text);
        }
    }

    /// Create a session and select it once the list refreshes.
    ///
    /// Requires a chosen workspace. Without one there is no honest answer to "where does
    /// this agent work", and the process cwd is the wrong guess: it is wherever the
    /// binary was launched from, not anywhere the human picked. So the picker opens
    /// instead, and the creation resumes once a directory is chosen.
    pub fn new_session(&mut self, transport: &mut Transport) {
        let Some(cwd) = self.workspace.clone() else {
            self.pending_new_session = true;
            self.record(
                Record::info("session.create.deferred")
                    .msg("no workspace chosen yet; opening the directory picker"),
            );
            self.browse_directory(None, transport);
            return;
        };
        // `workspaceId` **or** `cwd`, never both — the host rejects a request carrying
        // the two. Prefer the id: it is the only one that makes the host attach the
        // session to the workspace. With a bare `cwd` the session is created in the right
        // directory and joins no workspace, which is why one chosen by path alone stayed
        // permanently empty and every message started another session.
        let mut request = serde_json::Map::new();
        match self.workspace_id.clone() {
            Some(id) => request.insert("workspaceId".into(), Value::from(id)),
            None => request.insert("cwd".into(), Value::from(cwd.clone())),
        };
        let args = serde_json::json!({ "request": Value::Object(request) });
        match transport.call("session", "create", args) {
            Ok(id) => {
                self.create_call = Some(id);
                self.record(
                    Record::info("session.create")
                        .msg(format!("creating a session in {cwd}"))
                        .field("cwd", cwd),
                );
            }
            Err(error) => self.push_log(error.to_string()),
        }
    }

    /// Adopt the directory the picker is currently showing as the workspace.
    ///
    /// The directory being *browsed* is the one chosen, not the highlighted row: the row
    /// is where `→` would descend to, and choosing a directory you are looking into is
    /// what the breadcrumb in the title is describing.
    pub fn choose_directory(&mut self, transport: &mut Transport) {
        let Some(picker) = self.picker.as_ref() else { return };
        // Whatever the cursor is on: the current directory on the synthetic first row,
        // otherwise the highlighted subdirectory. One rule, so `enter` never acts on
        // something other than what is highlighted.
        let Some(path) = picker.chosen_path().map(str::to_string) else { return };
        if path.is_empty() {
            return;
        }
        self.close_picker();
        // `workspace/create` adopts an existing directory: the host registers it and
        // answers whether it was new or already known. Going through it is what makes a
        // choice outlive the run — the alternative, a path held only in memory, is
        // forgotten at exit and invisible to `^w`.
        let args = serde_json::json!({ "request": { "path": path.clone() } });
        match transport.call("workspace", "create", args) {
            Ok(id) => self.workspace_create = Some((id, path)),
            // The registration is best effort. A harness that does not expose the method
            // must not leave the human unable to start a session at all.
            Err(error) => {
                self.push_log(error.to_string());
                self.adopt_workspace(path, None, transport);
            }
        }
    }

    /// Take `path` as the session root.
    ///
    /// The one place the workspace is set, so every route into it — the picker, the
    /// `^w` list, the host's reply — logs the same way and resumes a deferred creation.
    fn adopt_workspace(
        &mut self,
        path: String,
        id: Option<String>,
        transport: &mut Transport,
    ) {
        let previous = self.workspace.replace(path.clone());
        // The caller's id wins: a workspace just registered is not in `workspaces` yet,
        // because its row arrives on a later frame. Falling back to the roster lookup
        // covers the callers that have no id of their own; taking the lookup *instead*
        // discarded the id the registration had just returned, and the session that
        // followed was then created by path and attached to nothing.
        self.workspace_id = id.or_else(|| {
            self.workspaces
                .rows()
                .iter()
                .find(|row| row.path == path)
                .map(|row| row.workspace_id.clone())
        });
        self.record(
            Record::info("workspace.chosen")
                .msg(format!("workspace set to {path}"))
                .field("path", path)
                .field("previous", previous.map(Value::from).unwrap_or(Value::Null)),
        );
        self.rescope_to_workspace(transport);
        // Choosing a workspace is the end of choosing and the start of working, so the
        // conversation takes the column back. Registering one from the `^w` list used to
        // leave that list on screen with no composer under it — the workspace was set and
        // there was nowhere to type.
        self.show_conversation();
        if std::mem::take(&mut self.pending_new_session) {
            self.new_session(transport);
        }
    }

    // ── the `^w` workspace list ──────────────────────────────────────────────

    pub fn open_workspace_list(&mut self, transport: &mut Transport) {
        self.open_workspace(transport);
        self.workspace_row = self
            .workspace_row
            .min(self.workspaces.rows().len().saturating_sub(1));
    }

    pub fn select_prev_workspace(&mut self) {
        self.workspace_row = self.workspace_row.saturating_sub(1);
    }

    pub fn select_next_workspace(&mut self) {
        self.workspace_row = (self.workspace_row + 1).min(self.last_workspace_row());
    }

    pub fn select_last_workspace(&mut self) {
        self.workspace_row = self.last_workspace_row();
    }

    fn last_workspace_row(&self) -> usize {
        self.workspaces.rows().len().saturating_sub(1)
    }

    /// Adopt the highlighted workspace and go back to the conversation.
    ///
    /// The host already knows this one, so there is nothing to register: its path is
    /// taken directly.
    pub fn use_selected_workspace(&mut self, transport: &mut Transport) {
        let rows = self.workspaces.rows();
        let Some(row) = rows.get(self.workspace_row) else { return };
        let path = row.path.clone();
        if path.is_empty() {
            return;
        }
        self.record(
            Record::info("workspace.selected")
                .msg(format!("selected {}", row.title))
                .field("workspaceId", row.workspace_id.clone())
                .field("path", path.clone()),
        );
        let id = row.workspace_id.clone();
        self.adopt_workspace(path, Some(id), transport);
    }

    /// Register a new workspace: pick a directory, then adopt it.
    pub fn create_workspace(&mut self, transport: &mut Transport) {
        self.browse_directory(None, transport);
    }

    /// The chosen workspace for display, with `$HOME` abbreviated.
    pub fn workspace_label(&self) -> Option<String> {
        let path = self.workspace.clone()?;
        let Ok(home) = std::env::var("HOME") else { return Some(path) };
        if home.is_empty() {
            return Some(path);
        }
        Some(match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path,
        })
    }

    /// Open the conversation search bar.
    pub fn open_search(&mut self) {
        self.search = Some(String::new());
        self.search_hits.clear();
        self.search_index = 0;
    }

    pub fn close_search(&mut self) {
        self.search = None;
        self.search_hits.clear();
    }

    /// Edit the search query and recompute its matches.
    pub fn edit_search(&mut self, edit: SearchEdit) {
        let Some(query) = self.search.as_mut() else { return };
        match edit {
            SearchEdit::Push(ch) => query.push(ch),
            SearchEdit::Backspace => {
                query.pop();
            }
            SearchEdit::Clear => query.clear(),
        }
        let query = query.clone();
        self.search_hits = viewport::matches(&self.rendered, &query);
        self.search_index = 0;
        self.reveal_current_match();
    }

    /// Move to the next or previous match, wrapping at either end.
    pub fn step_match(&mut self, forward: bool) {
        if self.search_hits.is_empty() {
            return;
        }
        let current = self
            .search_hits
            .get(self.search_index)
            .copied()
            .unwrap_or(0);
        let target = if forward {
            viewport::next_match(&self.search_hits, current)
        } else {
            viewport::prev_match(&self.search_hits, current)
        };
        if let Some(line) = target {
            self.search_index = self
                .search_hits
                .iter()
                .position(|hit| *hit == line)
                .unwrap_or(0);
            self.scroll.reveal(line);
        }
    }

    fn reveal_current_match(&mut self) {
        if let Some(line) = self.search_hits.get(self.search_index).copied() {
            self.scroll.reveal(line);
        }
    }

    /// The line the current match sits on, for highlighting.
    pub fn current_match(&self) -> Option<usize> {
        self.search_hits.get(self.search_index).copied()
    }

    /// Re-open the follow stream so its baseline replaces a ledger that cannot be trusted.
    fn repair_history(&mut self, transport: &mut Transport) {
        self.followed_session = None;
        if let Some(previous) = self.follow_stream.take() {
            let _ = transport.send(ClientMsg::Close { id: previous });
        }
        self.follow_selected(transport);
    }

    /// Move between the questions of a pending request.
    pub fn next_question(&mut self) {
        if let Some(pending) = self.questions.as_mut() {
            let count = pending.request.questions.len();
            if count > 0 {
                pending.question_index = (pending.question_index + 1).min(count - 1);
                pending.option_index = 0;
            }
        }
    }

    pub fn prev_question(&mut self) {
        if let Some(pending) = self.questions.as_mut() {
            pending.question_index = pending.question_index.saturating_sub(1);
            pending.option_index = 0;
        }
    }

    pub fn next_option(&mut self) {
        if let Some(pending) = self.questions.as_mut() {
            let count = pending.current().map(|q| q.options.len()).unwrap_or(0);
            if count > 0 {
                pending.option_index = (pending.option_index + 1).min(count - 1);
            }
        }
    }

    pub fn prev_option(&mut self) {
        if let Some(pending) = self.questions.as_mut() {
            pending.option_index = pending.option_index.saturating_sub(1);
        }
    }

    /// Choose the highlighted option for the current question.
    pub fn choose_option(&mut self) {
        let Some(pending) = self.questions.as_mut() else { return };
        let Some(question) = pending.current().cloned() else { return };
        let Some(option) = question.options.get(pending.option_index).cloned() else {
            return;
        };
        pending.draft.choose(&question, &option.label);
    }

    /// Submit the answers, releasing the blocked agent.
    pub fn submit_questions(&mut self, transport: &mut Transport) {
        let Some(pending) = self.questions.as_ref() else { return };
        if !pending.draft.is_complete(&pending.request) {
            return;
        }
        let value = pending.draft.encode(&pending.request);
        let id = pending.id;
        if let Err(error) = transport.send(ClientMsg::Answer { id, v: value }) {
            self.push_log(error.to_string());
        }
        self.questions = None;
    }

    /// Hand the request back to the host's own answerer.
    pub fn delegate_questions(&mut self, transport: &mut Transport) {
        let Some(pending) = self.questions.take() else { return };
        if let Err(error) = transport.send(ClientMsg::Next { id: pending.id }) {
            self.push_log(error.to_string());
        }
    }

    /// Answer the frontmost waterfall. The harness is blocked until this lands.
    pub fn answer_ask(&mut self, reply: AskReply, transport: &mut Transport) {
        let Some(ask) = self.asks.pop_front() else { return };
        let msg = match reply {
            AskReply::Allow => ClientMsg::Answer {
                id: ask.id,
                v: serde_json::json!({ "decision": "allow" }),
            },
            AskReply::Deny => ClientMsg::Answer {
                id: ask.id,
                v: serde_json::json!({ "decision": "deny" }),
            },
            AskReply::Delegate => ClientMsg::Next { id: ask.id },
        };
        if let Err(error) = transport.send(msg) {
            self.push_log(error.to_string());
        }
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        if !self.sidebar_open && self.focus == Pane::Sidebar {
            self.focus = Pane::Conversation;
        }
    }

    pub fn toggle_details(&mut self) {
        self.details_open = !self.details_open;
        if !self.details_open && self.focus == Pane::Details {
            self.focus = Pane::Conversation;
        }
    }

    pub fn focus_next(&mut self) {
        self.focus = self.focus.next(self.sidebar_open, self.details_open);
    }

    // Moving the cursor browses; `enter` opens. Following on every arrow tore down and
    // rebuilt a transcript stream per keypress, and left nothing for `enter` to do.

    pub fn select_prev_session(&mut self, _transport: &mut Transport) {
        let visible = self.visible_sessions();
        let at = visible.iter().position(|index| *index == self.selected_session);
        if let Some(previous) = at.and_then(|at| at.checked_sub(1)).and_then(|at| visible.get(at)) {
            self.selected_session = *previous;
        }
    }

    pub fn select_next_session(&mut self, _transport: &mut Transport) {
        let visible = self.visible_sessions();
        let at = visible.iter().position(|index| *index == self.selected_session);
        if let Some(next) = at.and_then(|at| visible.get(at + 1)) {
            self.selected_session = *next;
        }
    }

    /// The workspace a session belongs to, by the host's roster or by its own directory.
    pub fn workspace_of(&self, session: &SessionRow) -> Option<WorkspaceRow> {
        self.workspaces.rows().into_iter().find(|row| {
            row.session_ids.contains(&session.id)
                || Some(row.path.as_str()) == session.cwd.as_deref()
        })
    }

    /// The session the transcript is bound to, as a row.
    pub fn open_session(&self) -> Option<&SessionRow> {
        let open = self.followed_session.as_deref()?;
        self.sessions.iter().find(|row| row.id == open)
    }

    pub fn select_first_session(&mut self, _transport: &mut Transport) {
        if let Some(first) = self.visible_sessions().first() {
            self.selected_session = *first;
        }
    }

    pub fn select_last_session(&mut self, _transport: &mut Transport) {
        if let Some(last) = self.visible_sessions().last() {
            self.selected_session = *last;
        }
    }

    // ── workspace scoping ────────────────────────────────────────────────────

    /// Indices into `sessions` that the current workspace accounts for, oldest order kept.
    ///
    /// Everything, when there is no workspace or the host has not accounted this one — a
    /// workspace whose row has not arrived yet must not blank the sidebar, and a host that
    /// does not account sessions at all must not end up showing none.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let all = || (0..self.sessions.len()).collect::<Vec<_>>();
        let Some(workspace) = self.workspace.as_deref() else { return all() };
        let rows = self.workspaces.rows();
        // A path the host has not told us about: it cannot be scoped, so it is not
        // scoped. A workspace chosen before its row arrives must not blank the sidebar.
        let Some(row) = rows.iter().find(|row| row.path == workspace) else {
            return all();
        };
        // Two independent claims, because the host may make only one of them: the
        // workspace's own roster, and a session naming this directory as its `cwd`. A
        // session created here belongs here even when the workspace row has not caught
        // up — without the second claim the workspace looks permanently empty and every
        // message starts yet another session.
        let claimed: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                row.session_ids.contains(&session.id)
                    || session.cwd.as_deref() == Some(workspace)
            })
            .map(|(index, _)| index)
            .collect();
        if !claimed.is_empty() {
            return claimed;
        }
        // Nothing claims it. Either the host accounts sessions and this workspace is
        // genuinely empty, or it accounts nothing at all and scoping would hide
        // everything everywhere.
        let accounts = rows.iter().any(|row| !row.session_ids.is_empty())
            || self.sessions.iter().any(|session| session.cwd.is_some());
        if accounts { Vec::new() } else { all() }
    }

    /// Whether the current workspace has no sessions yet.
    ///
    /// Distinct from "no sessions at all": the host may have plenty, just none here.
    pub fn workspace_is_empty(&self) -> bool {
        self.workspace.is_some() && self.visible_sessions().is_empty()
    }

    /// Whether the sidebar is currently showing fewer sessions than the host has.
    pub fn sessions_are_scoped(&self) -> bool {
        self.visible_sessions().len() < self.sessions.len()
    }

    /// Move the cursor and the transcript into the current workspace.
    ///
    /// A workspace is a scope, so changing it has to change what is on screen: a
    /// transcript from a session the new workspace does not contain is not "the last thing
    /// you were reading", it is another project's conversation.
    fn rescope_to_workspace(&mut self, transport: &mut Transport) {
        let visible = self.visible_sessions();
        if visible.contains(&self.selected_session) {
            return;
        }
        if let Some(stream) = self.follow_stream.take() {
            let _ = transport.send(ClientMsg::Close { id: stream });
        }
        self.followed_session = None;
        self.ledger = Ledger::new();
        self.scroll = Viewport::new();
        self.close_search();
        match visible.first() {
            Some(first) => {
                self.selected_session = *first;
                self.follow_selected(transport);
            }
            // Nothing in this workspace yet: the conversation is empty on purpose, and
            // the sidebar's own hint says `n` starts one.
            None => self.selected_session = 0,
        }
    }
}

/// Read `/`-invocable skills into menu candidates.
fn parse_skills(value: &Value) -> Vec<Candidate> {
    let rows = value
        .get("skills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            let name = row.get("name").and_then(Value::as_str)?;
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // `whenToUse` is routing guidance the reader benefits from when choosing.
            let detail = match row.get("whenToUse").and_then(Value::as_str) {
                Some(when) if !when.is_empty() => format!("{description} — {when}"),
                _ => description.to_string(),
            };
            Some(Candidate {
                label: format!("/{name}"),
                detail,
                insert: format!("/{name}"),
            })
        })
        .collect()
}

/// How many namespaces a settings document holds.
fn describe_len(describe: Option<&Describe>) -> usize {
    describe.map(|d| d.namespaces.len()).unwrap_or(0)
}

/// One entry in the `/` or `@` menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What the row shows.
    pub label: String,
    /// Secondary text: a command's description, a file's directory.
    pub detail: String,
    /// The text that replaces the trigger span when picked.
    pub insert: String,
}

/// Read candidates from either menu's result shape.
///
/// `commands.list` returns `CommandDescriptor[]` (`name`, `description`);
/// `fileReferences.list` returns path candidates. One reader covers both so a new source
/// does not need a new branch.
fn parse_candidates(value: &Value) -> Vec<Candidate> {
    let rows = value
        .as_array()
        .cloned()
        .or_else(|| {
            value
                .get("candidates")
                .or_else(|| value.get("items"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();

    rows.iter()
        .filter_map(|row| {
            if let Some(name) = row.get("name").and_then(Value::as_str) {
                let description = row
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Some(Candidate {
                    label: format!("/{name}"),
                    detail: description.to_string(),
                    insert: format!("/{name}"),
                });
            }
            let path = row
                .get("path")
                .or_else(|| row.get("value"))
                .and_then(Value::as_str)
                .or_else(|| row.as_str())?;
            Some(Candidate {
                label: format!("@{path}"),
                detail: String::new(),
                insert: format!("@{path}"),
            })
        })
        .collect()
}

/// A question request taking over the composer. The agent is blocked until it settles.
#[derive(Debug)]
pub struct PendingQuestions {
    pub id: ExchangeId,
    pub request: QuestionRequest,
    pub draft: AnswerDraft,
    pub question_index: usize,
    pub option_index: usize,
}

impl PendingQuestions {
    /// The question currently in focus.
    pub fn current(&self) -> Option<&crate::questions::Question> {
        self.request.questions.get(self.question_index)
    }

    /// Whether every question has an answer, so the request can be submitted.
    pub fn is_complete(&self) -> bool {
        self.draft.is_complete(&self.request)
    }
}

/// Cell boxes reserved for staged images, and the payloads that fill them.
///
/// The frame reserves the cells; the payloads are written after the draw, because writing
/// first would let the buffer paint over the image.
fn image_row(terminal_width: u16, terminal_height: u16) -> CellRect {
    // Mirrors the frame layout with `conversation_height`: the chip strip sits one row
    // above the composer, inside the centre pane's border.
    CellRect {
        x: 31,
        y: terminal_height.saturating_sub(5),
        cols: terminal_width.saturating_sub(33).max(1),
        rows: 1,
    }
}

/// Visible conversation lines for a terminal of this height.
///
/// Mirrors the frame's own layout: one status row, the pane's two borders, the chip strip,
/// and the three-row composer.
fn conversation_height(terminal_height: u16) -> usize {
    usize::from(terminal_height).saturating_sub(7).max(1)
}

/// Visible rows in the log pane: the status bar, the pane border, and the two-line
/// header take the difference.
fn log_height(terminal_height: u16) -> usize {
    usize::from(terminal_height).saturating_sub(5).max(1)
}

/// Rows the directory picker can show, mirroring the popup geometry in `ui`.
///
/// The popup is centred and capped, so this is not simply the terminal height: borders
/// take two rows, the footer one, and the truncation note one more when it is shown.
fn picker_page(terminal_height: u16, truncated: bool) -> usize {
    let popup = usize::from(terminal_height).saturating_sub(4).clamp(1, 18);
    popup
        .saturating_sub(2 + 1 + usize::from(truncated))
        .max(1)
}

/// How far to scroll the conversation.
#[derive(Debug, Clone, Copy)]
pub enum ScrollDelta {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// One edit to a search box.
#[derive(Debug, Clone, Copy)]
pub enum SearchEdit {
    Push(char),
    Backspace,
    Clear,
}

/// What the human decided about a waterfall.
#[derive(Debug, Clone, Copy)]
pub enum AskReply {
    Allow,
    Deny,
    /// Hand the request back to the host's own listener.
    Delegate,
}

/// Namespaces and events the TUI cannot work without.
const REQUIRED_NAMESPACES: &[&str] = &["session", "workspace", "settings"];
const REQUIRED_EVENTS: &[&str] = &["approval/request", "user-questions/request"];

/// Fail the handshake loudly on drift, naming what is missing, rather than failing later
/// at first use with an opaque error.
fn missing_requirements(ready: &Ready) -> Option<String> {
    if ready.protocol != dsh_tui_proto::PROTOCOL_VERSION {
        return Some(format!(
            "bridge speaks protocol {} but this build speaks {}",
            ready.protocol,
            dsh_tui_proto::PROTOCOL_VERSION
        ));
    }
    let mut missing = Vec::new();
    for ns in REQUIRED_NAMESPACES {
        if !ready.namespaces.iter().any(|got| got == ns) {
            missing.push(format!("namespace {ns}"));
        }
    }
    for event in REQUIRED_EVENTS {
        if !ready.events.iter().any(|got| got == event) {
            missing.push(format!("event {event}"));
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!("bridge is missing {}", missing.join(", ")))
    }
}

/// Read session rows out of a `session.list` result.
///
/// `SessionListValue` is `{ items: SessionSummary[] }`, and a summary carries no title:
/// identity is `sessionId`, liveness is the boolean `running`, and the display title is
/// the `title` **projection**, which is `null` until the first title lands. A `blank`
/// summary is a provisional new session, which the web renderer shows as "New Session".
fn parse_sessions(value: &Value) -> Vec<SessionRow> {
    let rows = value
        .get("items")
        .or_else(|| value.get("sessions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    rows.iter()
        .filter_map(|row| {
            let id = row
                .get("sessionId")
                .or_else(|| row.get("id"))
                .and_then(Value::as_str)?;
            let projected_title = row
                .get("projections")
                .and_then(|hints| hints.get("values"))
                .and_then(|values| values.get("title"))
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty());
            let blank = row.get("blank").and_then(Value::as_bool).unwrap_or(false);
            let title = match (projected_title, blank) {
                (Some(title), _) => title.to_string(),
                // A blank session has not been used yet; "Untitled" would misdescribe it.
                (None, true) => "New session".to_string(),
                // Titled sessions get their title from a projection that may not have
                // landed yet; the id is the only honest stand-in meanwhile.
                (None, false) => id.to_string(),
            };
            Some(SessionRow {
                id: id.to_string(),
                title,
                running: row
                    .get("running")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                cwd: row
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                // A subagent session is listed but is not a top-level conversation.
                is_subagent: row.get("origin").and_then(Value::as_str) == Some("subagent"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(namespaces: &[&str], events: &[&str]) -> Ready {
        Ready {
            protocol: dsh_tui_proto::PROTOCOL_VERSION,
            client_id: None,
            host: None,
            namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
            events: events.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn handshake_accepts_a_complete_bridge() {
        let r = ready(
            &["session", "workspace", "settings"],
            &["approval/request", "user-questions/request"],
        );
        assert!(missing_requirements(&r).is_none());
    }

    #[test]
    fn handshake_names_what_drifted_away() {
        let r = ready(&["session"], &["approval/request"]);
        let message = missing_requirements(&r).expect("should reject");
        assert!(message.contains("namespace workspace"));
        assert!(message.contains("event user-questions/request"));
    }

    #[test]
    fn handshake_rejects_a_foreign_protocol_version() {
        let mut r = ready(
            &["session", "workspace", "settings"],
            &["approval/request", "user-questions/request"],
        );
        r.protocol = 99;
        assert!(missing_requirements(&r).unwrap().contains("protocol 99"));
    }

    #[test]
    fn focus_ring_skips_collapsed_panes() {
        // Details closed: conversation must wrap back to the sidebar, not into a hidden pane.
        assert_eq!(Pane::Conversation.next(true, false), Pane::Sidebar);
        assert_eq!(Pane::Sidebar.next(true, false), Pane::Conversation);
        // Everything closed but the middle: focus stays put rather than vanishing.
        assert_eq!(Pane::Conversation.next(false, false), Pane::Conversation);
    }

    #[test]
    fn unrecognized_waterfall_is_not_renderable() {
        let ask = PendingAsk {
            id: 1,
            event: "approval/request".into(),
            args: vec![serde_json::json!({ "somethingNew": true })],
        };
        assert!(!ask.is_renderable());
        assert!(ask.summary().contains("unrecognized shape"));
    }

}

#[cfg(test)]
mod session_list_tests {
    use super::*;

    #[test]
    fn summaries_parse_from_the_real_list_shape() {
        // `SessionListValue` is `{ items }`, identity is `sessionId`, liveness is the
        // boolean `running`, and there is no title field at all.
        let value = serde_json::json!({ "items": [
            { "sessionId": "s-1", "updatedAt": 1, "running": true, "blank": false,
              "cwd": "/home/acp/dsh-tui",
              "projections": { "asOfSeq": 9, "values": { "title": "Fix the parser" } } },
            { "sessionId": "s-2", "updatedAt": 2, "running": false, "blank": true },
            { "sessionId": "s-3", "updatedAt": 3, "running": false, "blank": false,
              "origin": "subagent" }
        ] });
        let rows = parse_sessions(&value);
        assert_eq!(rows.len(), 3);

        assert_eq!(rows[0].title, "Fix the parser");
        assert!(rows[0].running);
        assert_eq!(rows[0].cwd.as_deref(), Some("/home/acp/dsh-tui"));

        // A blank session has not been used yet; "Untitled" would misdescribe it.
        assert_eq!(rows[1].title, "New session");

        // A titled session whose title projection has not landed falls back to its id
        // rather than inventing a name.
        assert_eq!(rows[2].title, "s-3");
        assert!(rows[2].is_subagent);
    }

    #[test]
    fn a_null_title_projection_is_not_treated_as_a_title() {
        let value = serde_json::json!({ "items": [
            { "sessionId": "s-1", "running": false, "blank": false,
              "projections": { "asOfSeq": 0, "values": { "title": null } } }
        ] });
        let rows = parse_sessions(&value);
        assert_eq!(rows[0].title, "s-1");
    }
}
