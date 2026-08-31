//! Structured logging: one NDJSON record per event, written to a per-run file and handed
//! back to the caller for the in-app log pane.
//!
//! Both ends of the stack emit the *same* record shape — the Rust TUI directly, the
//! bridge as `@dsh-log `-prefixed lines on stderr that [`Record::parse_bridge_line`]
//! reads back — and both stamp the exchange id. One file therefore shows a request
//! leaving the TUI, what the bridge did with it, and what came back, in order.
//!
//! A TUI cannot log to the terminal: stdout carries image escapes and the alternate
//! screen owns the display. The file is the log; the pane is a view of it.
//!
//! ## Configuration
//!
//! | Variable | Default | Meaning |
//! | --- | --- | --- |
//! | `DSH_TUI_LOG` | `info` | `error` \| `warn` \| `info` \| `debug` \| `trace` \| `off` |
//! | `DSH_TUI_LOG_PAYLOADS` | unset | `1` records full frame bodies rather than shapes |
//! | `DSH_TUI_LOG_DIR` | see below | where per-run files land |
//!
//! The default directory is `$XDG_STATE_HOME/dsh-tui`, falling back to
//! `$HOME/.local/state/dsh-tui`; on Windows it is `%LOCALAPPDATA%\\dsh-tui`. Each run
//! writes `run-<millis>-<pid>.ndjson`, and on Unix `latest.ndjson` symlinks to the
//! current one — Windows has no such link, so read the newest `run-*` file there.
//! | `DSH_TUI_LOG_FILE` | — | exact file to write, overriding the directory |
//! | `DSH_TUI_LOG_MAX_BYTES` | `67108864` | per-run cap; the file stops growing after it |
//! | `DSH_TUI_LOG_KEEP` | `20` | how many previous runs survive pruning |

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// The prefix the bridge puts in front of a structured stderr line.
///
/// Bridge stderr also carries the harness host's own output, which is arbitrary text; the
/// prefix is what separates a record we can read from a line we can only display.
pub const BRIDGE_PREFIX: &str = "@dsh-log ";

// ── levels ───────────────────────────────────────────────────────────────────

/// Severity, ordered so `level <= threshold` means "record it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    /// Fixed-width tag for the pane, so records align regardless of severity.
    pub fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERR ",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DBG ",
            Level::Trace => "TRC ",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "error" | "err" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" | "dbg" => Some(Level::Debug),
            "trace" | "trc" => Some(Level::Trace),
            _ => None,
        }
    }

    /// The next level up the ladder, wrapping. Drives the pane's filter key.
    pub fn cycle(self) -> Self {
        match self {
            Level::Error => Level::Warn,
            Level::Warn => Level::Info,
            Level::Info => Level::Debug,
            Level::Debug => Level::Trace,
            Level::Trace => Level::Error,
        }
    }
}

/// Which side opened an exchange.
///
/// Client- and bridge-originated ids are independent number spaces — PROTOCOL.md says so
/// — which means a bare id does not identify an exchange. `c1` and `s1` are two different
/// things, and a log that called both `#1` would splice a call and a waterfall together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The TUI opened it: a call, or a stream.
    Client,
    /// The bridge opened it: a waterfall blocking the agent.
    Bridge,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Client => "c",
            Origin::Bridge => "s",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "s" => Origin::Bridge,
            _ => Origin::Client,
        }
    }
}

/// Which half of the stack produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The Rust TUI.
    Tui,
    /// The `dsh-tui-bridge` Node process.
    Bridge,
    /// The harness host behind the bridge, whose output is unstructured text.
    Host,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Tui => "tui",
            Source::Bridge => "bridge",
            Source::Host => "host",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "bridge" => Source::Bridge,
            "host" => Source::Host,
            _ => Source::Tui,
        }
    }
}

// ── records ──────────────────────────────────────────────────────────────────

/// One logged event.
#[derive(Debug, Clone)]
pub struct Record {
    /// Wall clock, epoch milliseconds. Absolute so the file lines up with harness logs.
    pub ts: u64,
    pub level: Level,
    pub src: Source,
    /// Dotted event name — `frame.out`, `call.err`, `ui.key`. Stable, greppable, and the
    /// thing to group by when reading the flow rather than one incident.
    pub event: String,
    /// The exchange this record belongs to, with the side that opened it. Together they
    /// are the correlation key across both ends.
    pub id: Option<u64>,
    pub origin: Option<Origin>,
    /// One-line human summary. What the pane shows.
    pub message: String,
    /// Structured detail. What the analysis reads.
    pub fields: Map<String, Value>,
}

impl Record {
    pub fn new(level: Level, event: impl Into<String>) -> Self {
        Self {
            ts: now_ms(),
            level,
            src: Source::Tui,
            event: event.into(),
            id: None,
            origin: None,
            message: String::new(),
            fields: Map::new(),
        }
    }

    pub fn error(event: impl Into<String>) -> Self {
        Self::new(Level::Error, event)
    }
    pub fn warn(event: impl Into<String>) -> Self {
        Self::new(Level::Warn, event)
    }
    pub fn info(event: impl Into<String>) -> Self {
        Self::new(Level::Info, event)
    }
    pub fn debug(event: impl Into<String>) -> Self {
        Self::new(Level::Debug, event)
    }
    pub fn trace(event: impl Into<String>) -> Self {
        Self::new(Level::Trace, event)
    }

    pub fn src(mut self, src: Source) -> Self {
        self.src = src;
        self
    }

    /// Tag the record with the exchange it belongs to.
    ///
    /// Both halves are required: an id without its originator does not identify an
    /// exchange, and callers that were allowed to give only the number would drift back
    /// into conflating the two number spaces.
    pub fn exchange(mut self, origin: Origin, id: u64) -> Self {
        self.id = Some(id);
        self.origin = Some(origin);
        self
    }

    pub fn msg(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// Attach a field verbatim. For anything that crossed the wire use [`Self::payload`].
    pub fn field(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.fields.insert(key.to_string(), value.into());
        self
    }

    /// Attach a wire payload, redacted unless full capture is on.
    pub fn payload(mut self, key: &str, value: &Value) -> Self {
        self.fields.insert(key.to_string(), describe(value));
        self
    }

    /// Serialize to the NDJSON line both ends write.
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("ts".into(), Value::from(self.ts));
        object.insert("lvl".into(), Value::from(self.level.as_str()));
        object.insert("src".into(), Value::from(self.src.as_str()));
        object.insert("ev".into(), Value::from(self.event.clone()));
        if let Some(id) = self.id {
            object.insert("id".into(), Value::from(id));
        }
        if let Some(origin) = self.origin {
            object.insert("org".into(), Value::from(origin.as_str()));
        }
        if !self.message.is_empty() {
            object.insert("msg".into(), Value::from(self.message.clone()));
        }
        if !self.fields.is_empty() {
            object.insert("f".into(), Value::Object(self.fields.clone()));
        }
        Value::Object(object)
    }

    /// Read a record the bridge wrote to stderr.
    ///
    /// Returns `None` for any line that is not one of ours — the harness host's own
    /// output shares the stream, and it must be shown, not parsed.
    pub fn parse_bridge_line(line: &str) -> Option<Self> {
        let body = line.trim_start().strip_prefix(BRIDGE_PREFIX)?;
        let value: Value = serde_json::from_str(body.trim()).ok()?;
        let object = value.as_object()?;
        let get_str = |key: &str| object.get(key).and_then(Value::as_str);
        Some(Self {
            ts: object.get("ts").and_then(Value::as_u64).unwrap_or_else(now_ms),
            level: get_str("lvl").and_then(Level::parse).unwrap_or(Level::Info),
            // The bridge names itself, so a record it *forwarded* from the host keeps
            // that provenance instead of being relabelled.
            src: get_str("src").map(Source::parse).unwrap_or(Source::Bridge),
            event: get_str("ev").unwrap_or("bridge").to_string(),
            id: object.get("id").and_then(Value::as_u64),
            origin: get_str("org").map(Origin::parse),
            message: get_str("msg").unwrap_or_default().to_string(),
            fields: object
                .get("f")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        })
    }

    /// A line of unstructured output, kept as a record so one pane shows everything.
    pub fn from_raw_line(line: &str) -> Self {
        Self::new(Level::Info, "host.stderr")
            .src(Source::Host)
            .msg(scrub_tokens(line.trim_end()))
    }

    /// Whether `needle` appears anywhere in the record. Backs the pane's filter, which
    /// searches the structured fields too — an exchange id is often all one has.
    pub fn contains(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let needle = needle.to_ascii_lowercase();
        if self.message.to_ascii_lowercase().contains(&needle)
            || self.event.to_ascii_lowercase().contains(&needle)
            || self.level.as_str().contains(&needle)
            || self.src.as_str().contains(&needle)
        {
            return true;
        }
        // Both `1` and `c1` find the exchange, so a number read off one row is enough
        // to pull up the rest of it.
        if let Some(id) = self.id {
            if needle == id.to_string() {
                return true;
            }
            if let Some(origin) = self.origin {
                if needle == format!("{}{id}", origin.as_str()) {
                    return true;
                }
            }
        }
        Value::Object(self.fields.clone())
            .to_string()
            .to_ascii_lowercase()
            .contains(&needle)
    }

    /// The pane's one-line rendering: elapsed time, level, source, event, then detail.
    pub fn render(&self, start_ms: u64) -> String {
        let mut line = format!(
            "{} {} {:<6} {}",
            elapsed(self.ts, start_ms),
            self.level.tag(),
            self.src.as_str(),
            self.event
        );
        if let Some(id) = self.id {
            let origin = self.origin.map(Origin::as_str).unwrap_or("?");
            line.push_str(&format!(" #{origin}{id}"));
        }
        if !self.message.is_empty() {
            line.push_str("  ");
            line.push_str(&self.message);
        }
        if !self.fields.is_empty() {
            line.push_str("  ");
            line.push_str(&Value::Object(self.fields.clone()).to_string());
        }
        line
    }
}

/// `+m:ss.mmm` since the TUI started. Relative beats wall clock in a pane: reading a flow
/// means reading gaps, and the file keeps the absolute timestamp for correlation.
fn elapsed(ts: u64, start_ms: u64) -> String {
    let delta = ts.saturating_sub(start_ms);
    let millis = delta % 1000;
    let seconds = (delta / 1000) % 60;
    let minutes = delta / 60_000;
    format!("+{minutes:>3}:{seconds:02}.{millis:03}")
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── redaction ────────────────────────────────────────────────────────────────

/// Keys whose values never reach the log, whatever the payload setting.
///
/// The flag exists to analyze the flow, not to spill credentials into a file that outlives
/// the session. `token` is matched exactly so token *counts* — `inputTokens` — survive.
const SECRET_EXACT: &[&str] = &[
    "token",
    "accesstoken",
    "access_token",
    "refreshtoken",
    "refresh_token",
    "apikey",
    "api_key",
    "secret",
    "password",
    "passwd",
    "authorization",
    "credential",
    "credentials",
    "bearer",
    "privatekey",
    "private_key",
];

/// Substrings that make a key secret wherever they appear (`openaiApiKey`, `clientSecret`).
const SECRET_CONTAINS: &[&str] = &["apikey", "api_key", "secret", "password", "credential"];

/// Query parameters whose values are stripped out of unstructured host output.
///
/// The harness prints URLs carrying a launch token. That was harmless when it scrolled
/// past on stderr; it is not harmless in a file that outlives the session.
const SECRET_PARAMS: &[&str] = &["token=", "access_token=", "key=", "secret="];

/// Replace `?token=…` and friends in a line of arbitrary text.
///
/// Hand-rolled rather than a regex: this crate carries no regex dependency, and the shape
/// being matched is a fixed set of literal prefixes ending at the next delimiter.
///
/// Scans forward once. Rewriting in place and re-scanning from the start would keep
/// re-matching the replacement, which never terminates.
pub fn scrub_tokens(line: &str) -> String {
    const PLACEHOLDER: &str = "<redacted>";
    let lower = line.to_ascii_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;

    while cursor < line.len() {
        // The earliest parameter at or after the cursor that still has a value.
        let found = SECRET_PARAMS
            .iter()
            .filter_map(|param| {
                lower[cursor..].match_indices(param).find_map(|(offset, _)| {
                    let at = cursor + offset;
                    // Only inside a URL query or fragment, so prose containing "key=" is
                    // left alone.
                    let prefix = line.as_bytes().get(at.checked_sub(1)?)?;
                    if !matches!(prefix, b'?' | b'&' | b'#') {
                        return None;
                    }
                    let value = at + param.len();
                    let stop = line[value..]
                        .find(|c: char| c.is_whitespace() || c == '&' || c == '#')
                        .map(|offset| value + offset)
                        .unwrap_or(line.len());
                    if stop > value {
                        Some((value, stop))
                    } else {
                        None
                    }
                })
            })
            .min_by_key(|(value, _)| *value);

        let Some((value, stop)) = found else { break };
        out.push_str(&line[cursor..value]);
        out.push_str(PLACEHOLDER);
        cursor = stop;
    }
    out.push_str(&line[cursor..]);
    out
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_EXACT.contains(&lower.as_str())
        || SECRET_CONTAINS.iter().any(|needle| lower.contains(needle))
}

/// Longest string kept verbatim when payloads are redacted.
///
/// Short strings are almost always structural — an event kind, a status, a session id —
/// and dropping them makes a redacted log useless. Prose and file bodies are longer.
const SHORT_STRING: usize = 48;

const MAX_DEPTH: usize = 8;
const MAX_KEYS: usize = 64;
const MAX_ITEMS: usize = 8;

/// Reduce a wire payload to what the log should hold.
///
/// Full capture (`DSH_TUI_LOG_PAYLOADS=1`) keeps the body, minus secrets and minus
/// whatever exceeds the size cap. Otherwise the value is reduced to its *shape*: keys and
/// types survive, long strings become `str(N)`, so a flow stays readable without the
/// prompts and file contents that produced it.
pub fn describe(value: &Value) -> Value {
    if payloads_enabled() {
        let scrubbed = scrub(value, 0);
        let cap = max_bytes_per_value();
        // Serializing twice is cheaper than the alternative of capping while walking, and
        // this only runs when the operator asked for full bodies.
        let rendered = scrubbed.to_string();
        if rendered.len() > cap {
            return Value::Object(Map::from_iter([
                ("$elided".to_string(), Value::from("payload over cap")),
                ("$bytes".to_string(), Value::from(rendered.len())),
                ("$shape".to_string(), shape(value, 0)),
            ]));
        }
        return scrubbed;
    }
    shape(value, 0)
}

/// The value as-is, with secret-keyed entries replaced.
fn scrub(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DEPTH {
        return Value::from("…");
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| {
                    let child = if is_secret_key(key) {
                        Value::from("<redacted>")
                    } else {
                        scrub(child, depth + 1)
                    };
                    (key.clone(), child)
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| scrub(item, depth + 1)).collect())
        }
        other => other.clone(),
    }
}

/// Keys and types, with content elided.
fn shape(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DEPTH {
        return Value::from("…");
    }
    match value {
        Value::Null => Value::from("null"),
        Value::Bool(flag) => Value::from(*flag),
        Value::Number(number) => Value::Number(number.clone()),
        Value::String(text) => {
            if text.chars().count() <= SHORT_STRING {
                Value::from(text.clone())
            } else {
                Value::from(format!("str({})", text.len()))
            }
        }
        Value::Array(items) => {
            let mut out: Vec<Value> = items
                .iter()
                .take(MAX_ITEMS)
                .map(|item| shape(item, depth + 1))
                .collect();
            if items.len() > MAX_ITEMS {
                out.push(Value::from(format!("…+{} more", items.len() - MAX_ITEMS)));
            }
            Value::Array(out)
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map.iter().take(MAX_KEYS) {
                let child = if is_secret_key(key) {
                    Value::from("<redacted>")
                } else {
                    shape(child, depth + 1)
                };
                out.insert(key.clone(), child);
            }
            if map.len() > MAX_KEYS {
                out.insert(
                    "…".to_string(),
                    Value::from(format!("+{} more keys", map.len() - MAX_KEYS)),
                );
            }
            Value::Object(out)
        }
    }
}

// ── the sink ─────────────────────────────────────────────────────────────────

struct Sink {
    level: Level,
    payloads: bool,
    max_bytes: usize,
    path: PathBuf,
    writer: Mutex<Writer>,
    started: u64,
}

struct Writer {
    file: Option<File>,
    written: usize,
    capped: bool,
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();

fn sink() -> Option<&'static Sink> {
    SINK.get().and_then(Option::as_ref)
}

/// Set up logging for this run.
///
/// Returns the file being written, if any. Never fails the program: a TUI that cannot
/// open a log file must still start, so a broken destination degrades to the pane alone.
pub fn init() -> Option<PathBuf> {
    let configured = SINK.get_or_init(build_sink);
    configured.as_ref().map(|sink| sink.path.clone())
}

fn build_sink() -> Option<Sink> {
    let level = match std::env::var("DSH_TUI_LOG") {
        Ok(text) if text.trim().eq_ignore_ascii_case("off") => return None,
        Ok(text) => Level::parse(&text).unwrap_or(Level::Info),
        Err(_) => Level::Info,
    };
    let payloads = env_flag("DSH_TUI_LOG_PAYLOADS");
    let max_bytes = std::env::var("DSH_TUI_LOG_MAX_BYTES")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(64 * 1024 * 1024);

    let path = destination()?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    // A stable name to `tail -f`, and a bounded directory. Best effort: neither is worth
    // failing a run over.
    if let Some(parent) = path.parent() {
        link_latest(parent, &path);
        prune(parent, keep_runs());
    }

    Some(Sink {
        level,
        payloads,
        max_bytes,
        path,
        writer: Mutex::new(Writer {
            file: Some(file),
            written: 0,
            capped: false,
        }),
        started: now_ms(),
    })
}

fn destination() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DSH_TUI_LOG_FILE") {
        if !explicit.trim().is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    let dir = match std::env::var("DSH_TUI_LOG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => state_dir()?,
    };
    Some(dir.join(format!("run-{}-{}.ndjson", now_ms(), std::process::id())))
}

fn state_dir() -> Option<PathBuf> {
    state_dir_from(cfg!(windows), |name| std::env::var(name).ok())
}

/// Where per-run logs land, resolved from the environment.
///
/// Split out from [`state_dir`], and given the platform as an argument rather than reading
/// `cfg!` directly, so both resolution orders are testable from either host without
/// mutating the process environment — which is global and shared with every other test in
/// the binary.
///
/// Windows sets neither `XDG_STATE_HOME` nor `HOME`, so a `HOME`-only fallback returns
/// `None` there and the run logs nothing at all. `%LOCALAPPDATA%` is where per-user state
/// belongs on Windows; it is preferred over `HOME` so the log lands in the same place
/// whether the binary was started from PowerShell, cmd, or a Git Bash that sets `HOME`
/// too. `HOME` stays the last resort on both platforms.
fn state_dir_from(windows: bool, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |name: &str| -> Option<PathBuf> {
        let value = env(name)?;
        if value.trim().is_empty() {
            return None;
        }
        Some(PathBuf::from(value))
    };

    if let Some(state) = var("XDG_STATE_HOME") {
        return Some(state.join("dsh-tui"));
    }
    if windows {
        if let Some(local) = var("LOCALAPPDATA") {
            return Some(local.join("dsh-tui"));
        }
        if let Some(profile) = var("USERPROFILE") {
            return Some(profile.join("AppData/Local/dsh-tui"));
        }
    }
    Some(var("HOME")?.join(".local/state/dsh-tui"))
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn keep_runs() -> usize {
    std::env::var("DSH_TUI_LOG_KEEP")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(20)
}

fn link_latest(dir: &Path, target: &Path) {
    let link = dir.join("latest.ndjson");
    let _ = fs::remove_file(&link);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(target, &link);
}

/// Keep the newest `keep` run files. A log directory that grows without bound is a bug
/// report nobody files and a disk nobody notices filling.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut runs: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("run-") || !name.ends_with(".ndjson") {
                return None;
            }
            // Sort on the name's own timestamp rather than mtime: the name is what the
            // run stamped, and it stays stable if the files are ever copied.
            let stamp = name.trim_start_matches("run-").split('-').next()?.parse().ok()?;
            Some((stamp, path))
        })
        .collect();
    if runs.len() <= keep {
        return;
    }
    runs.sort_by_key(|(stamp, _)| *stamp);
    for (_, path) in runs.iter().take(runs.len() - keep) {
        let _ = fs::remove_file(path);
    }
}

/// Whether a record at `level` would be kept. Guard expensive field building with this.
pub fn enabled(level: Level) -> bool {
    match SINK.get() {
        // Before `init`, nothing is filtered: tests and the screenshot path build records
        // for the pane without ever opening a file.
        None => true,
        Some(None) => false,
        Some(Some(sink)) => level <= sink.level,
    }
}

fn payloads_enabled() -> bool {
    sink().map(|sink| sink.payloads).unwrap_or(false)
}

fn max_bytes_per_value() -> usize {
    // One record must not be able to fill the whole budget.
    sink().map(|sink| sink.max_bytes / 64).unwrap_or(64 * 1024)
}

/// The file this run is writing, once [`init`] has run.
pub fn path() -> Option<PathBuf> {
    sink().map(|sink| sink.path.clone())
}

/// When this run started, epoch milliseconds. The pane renders times relative to it.
pub fn started() -> u64 {
    sink().map(|sink| sink.started).unwrap_or(0)
}

/// The configured threshold, for display.
pub fn level() -> Level {
    sink().map(|sink| sink.level).unwrap_or(Level::Info)
}

/// Whether full payloads are being captured, for display.
pub fn payloads() -> bool {
    payloads_enabled()
}

/// Write `record` to the log file and hand it back for the pane.
///
/// `None` means the record was filtered: the caller should not show it either, so the
/// pane and the file never disagree about what this run recorded.
pub fn emit(record: Record) -> Option<Record> {
    if !enabled(record.level) {
        return None;
    }
    if let Some(sink) = sink() {
        if let Ok(mut writer) = sink.writer.lock() {
            write_record(&mut writer, sink.max_bytes, &record);
        }
    }
    Some(record)
}

fn write_record(writer: &mut Writer, max_bytes: usize, record: &Record) {
    if writer.capped {
        return;
    }
    let Some(file) = writer.file.as_mut() else { return };
    let line = format!("{}\n", record.to_json());
    if writer.written + line.len() > max_bytes {
        // Say so in the file itself: a log that stops without explanation reads as a
        // crash, and someone will go looking for one.
        let notice = Record::warn("log.capped")
            .field("bytes", writer.written)
            .msg("log size cap reached; no further records written");
        let _ = file.write_all(format!("{}\n", notice.to_json()).as_bytes());
        let _ = file.flush();
        writer.capped = true;
        return;
    }
    if file.write_all(line.as_bytes()).is_ok() {
        writer.written += line.len();
    }
    // Flushed every record: the interesting run is the one that ended in a crash, and a
    // buffered tail is exactly the part that would be missing.
    let _ = file.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn short_strings_survive_redaction_and_long_ones_do_not() {
        // A redacted log is only useful if the structural strings stay readable.
        let value = json!({ "type": "tool/result", "text": "x".repeat(400) });
        let shaped = shape(&value, 0);
        assert_eq!(shaped["type"], json!("tool/result"));
        assert_eq!(shaped["text"], json!("str(400)"));
    }

    #[test]
    fn secrets_are_dropped_even_when_full_capture_is_on() {
        // `scrub` is the full-capture path; the flag must not be a way to log a key.
        let value = json!({ "openaiApiKey": "sk-live", "nested": { "password": "hunter2" } });
        let scrubbed = scrub(&value, 0);
        assert_eq!(scrubbed["openaiApiKey"], json!("<redacted>"));
        assert_eq!(scrubbed["nested"]["password"], json!("<redacted>"));
    }

    #[test]
    fn token_counts_are_not_mistaken_for_credentials() {
        // `token` is matched exactly, so usage numbers stay in the log.
        let value = json!({ "inputTokens": 12, "token": "abc" });
        let scrubbed = scrub(&value, 0);
        assert_eq!(scrubbed["inputTokens"], json!(12));
        assert_eq!(scrubbed["token"], json!("<redacted>"));
    }

    #[test]
    fn windows_logs_to_local_appdata_rather_than_nowhere() {
        // Windows sets neither XDG_STATE_HOME nor HOME; before the fallback existed this
        // resolved to None and the whole run went unlogged.
        let env = |name: &str| match name {
            "LOCALAPPDATA" => Some(r"C:\Users\a\AppData\Local".to_string()),
            "USERPROFILE" => Some(r"C:\Users\a".to_string()),
            _ => None,
        };
        assert_eq!(
            state_dir_from(true, env),
            Some(PathBuf::from(r"C:\Users\a\AppData\Local").join("dsh-tui"))
        );
    }

    #[test]
    fn windows_falls_back_to_the_user_profile_when_local_appdata_is_cleared() {
        let env = |name: &str| match name {
            "USERPROFILE" => Some(r"C:\Users\a".to_string()),
            _ => None,
        };
        assert_eq!(
            state_dir_from(true, env),
            Some(PathBuf::from(r"C:\Users\a").join("AppData/Local/dsh-tui"))
        );
    }

    #[test]
    fn a_git_bash_home_does_not_move_the_windows_log_directory() {
        // Both are set under Git Bash. Preferring LOCALAPPDATA keeps one location per
        // machine rather than one per shell the binary happened to be started from.
        let env = |name: &str| match name {
            "LOCALAPPDATA" => Some(r"C:\Users\a\AppData\Local".to_string()),
            "HOME" => Some("/c/Users/a".to_string()),
            _ => None,
        };
        assert_eq!(
            state_dir_from(true, env),
            Some(PathBuf::from(r"C:\Users\a\AppData\Local").join("dsh-tui"))
        );
    }

    #[test]
    fn unix_ignores_the_windows_variables_and_an_empty_one_is_not_a_path() {
        // An exported-but-empty XDG_STATE_HOME must not resolve to `/dsh-tui`.
        let env = |name: &str| match name {
            "XDG_STATE_HOME" => Some("   ".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\a\AppData\Local".to_string()),
            "HOME" => Some("/home/a".to_string()),
            _ => None,
        };
        assert_eq!(
            state_dir_from(false, env),
            Some(PathBuf::from("/home/a/.local/state/dsh-tui"))
        );
    }

    #[test]
    fn nothing_in_the_environment_means_no_log_file() {
        assert_eq!(state_dir_from(true, |_| None), None);
        assert_eq!(state_dir_from(false, |_| None), None);
    }

    #[test]
    fn a_bridge_line_round_trips_through_the_prefix() {
        let record = Record::info("call.ok")
            .src(Source::Bridge)
            .exchange(Origin::Client, 7)
            .msg("done");
        let line = format!("{}{}", BRIDGE_PREFIX, record.to_json());
        let back = Record::parse_bridge_line(&line).expect("prefixed line should parse");
        assert_eq!(back.event, "call.ok");
        assert_eq!(back.id, Some(7));
        assert_eq!(back.origin, Some(Origin::Client));
        assert_eq!(back.src, Source::Bridge);
    }

    #[test]
    fn the_two_id_spaces_do_not_collide() {
        // A call the TUI opened and a waterfall the bridge opened can both be id 1.
        let call = Record::debug("call.out").exchange(Origin::Client, 1);
        let ask = Record::info("ask").exchange(Origin::Bridge, 1);
        assert_ne!(call.to_json()["org"], ask.to_json()["org"]);
        assert!(call.render(0).contains("#c1"));
        assert!(ask.render(0).contains("#s1"));
        // `c1` selects one of them; the bare number still finds both.
        assert!(call.contains("c1") && !ask.contains("c1"));
        assert!(call.contains("1") && ask.contains("1"));
    }

    #[test]
    fn a_launch_token_never_reaches_the_file() {
        // The harness prints its URL, and the file outlives the session.
        let record = Record::from_raw_line("dsh web: http://127.0.0.1:34313/?token=s3cret-abc");
        assert!(record.message.contains("token=<redacted>"), "{}", record.message);
        assert!(!record.message.contains("s3cret"));
        // The rest of the line survives — the port is the useful part.
        assert!(record.message.contains("127.0.0.1:34313"));
    }

    #[test]
    fn more_than_one_parameter_is_stripped_and_ordinary_prose_is_not() {
        let line = "GET /a?token=aaa&access_token=bbb&page=2 -> 200";
        assert_eq!(
            scrub_tokens(line),
            "GET /a?token=<redacted>&access_token=<redacted>&page=2 -> 200"
        );
        // `key=` outside a query string is prose, not a credential.
        assert_eq!(scrub_tokens("cache key=abc missing"), "cache key=abc missing");
    }

    #[test]
    fn host_output_is_not_parsed_as_a_record() {
        // The harness host shares stderr and prints whatever it likes, including JSON.
        assert!(Record::parse_bridge_line(r#"{"ts":1,"lvl":"info"}"#).is_none());
        assert!(Record::parse_bridge_line("listening on 127.0.0.1:0").is_none());
    }

    #[test]
    fn the_filter_reaches_into_fields_and_ids() {
        let record = Record::info("frame.out")
            .exchange(Origin::Client, 42)
            .field("ns", "session");
        assert!(record.contains("session"));
        assert!(record.contains("42"));
        assert!(!record.contains("workspace"));
    }
}
