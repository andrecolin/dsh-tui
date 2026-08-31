//! Wire types for the dsh-tui protocol.
//!
//! Newline-delimited JSON over the harness child's stdin/stdout. Rust is the client, the
//! `dsh-tui-bridge` plugin is the server, and both sides originate messages: the harness
//! blocks on the TUI for permission approvals and user questions.
//!
//! See `PROTOCOL.md` at the repository root for the normative description.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol major version. The bridge refuses a client announcing a different major.
pub const PROTOCOL_VERSION: u32 = 1;

/// Identifies one call, stream, or waterfall exchange. Client-originated and
/// bridge-originated ids occupy independent spaces and may collide harmlessly.
pub type ExchangeId = u64;

/// A physical carrier generation. An increment invalidates prior state for that stream:
/// every generation opens with a complete baseline rather than a delta.
pub type Generation = u64;

// ── client → bridge ──────────────────────────────────────────────────────────

/// A message the TUI sends to the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientMsg {
    /// Invoke `ctx.remote.<ns>.<m>(args)` and await one result.
    Call {
        id: ExchangeId,
        ns: String,
        m: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
    },
    /// Abort an in-flight call. Cancelling an unknown or settled id is a no-op.
    Cancel { id: ExchangeId },
    /// Open a journal or snapshot stream.
    Open {
        id: ExchangeId,
        stream: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
    },
    /// Dispose a stream. The bridge answers with `End`.
    Close { id: ExchangeId },
    /// Resolve a waterfall with a result.
    Answer { id: ExchangeId, v: Value },
    /// Decline to answer a waterfall and delegate to the next host listener.
    ///
    /// Not the same as denying: a TUI that cannot render a request shape must delegate
    /// rather than invent a decision on the user's behalf.
    Next { id: ExchangeId },
    /// Fail a waterfall.
    Reject { id: ExchangeId, message: String },
    /// Ask the runtime to shut down cleanly.
    Shutdown,
}

// ── bridge → client ──────────────────────────────────────────────────────────

/// A message the bridge sends to the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ServerMsg {
    /// Sent once the client face has mounted and every forwarded-event listener is
    /// attached, so no event can be lost between mount and first read.
    Ready(Ready),
    /// A call succeeded.
    Ok { id: ExchangeId, v: Value },
    /// A call failed. `code` preserves the wire RPC code, so a policy rejection stays
    /// distinguishable from a generic `internal`.
    Err {
        id: ExchangeId,
        code: String,
        message: String,
        #[serde(default)]
        data: Option<Value>,
    },
    /// One stream item, tagged with the generation that produced it.
    Item {
        id: ExchangeId,
        gen: Generation,
        v: Value,
    },
    /// A stream ended normally.
    End {
        id: ExchangeId,
        #[serde(default)]
        reason: Option<String>,
    },
    /// A stream failed terminally.
    StreamErr {
        id: ExchangeId,
        code: String,
        message: String,
    },
    /// A one-way forwarded host event. Not replayed after reconnect.
    Event { event: String, args: Vec<Value> },
    /// A waterfall request. The harness is blocked until exactly one of `Answer`,
    /// `Next`, or `Reject` carries this id back.
    Ask {
        id: ExchangeId,
        event: String,
        #[serde(default)]
        agent: Option<Value>,
        args: Vec<Value>,
    },
    /// Acknowledges `Shutdown`.
    Bye,
}

/// The opening frame's payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    pub protocol: u32,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub host: Option<HostFacts>,
    /// Remote namespaces the bridge mounted.
    #[serde(default)]
    pub namespaces: Vec<String>,
    /// Forwarded events the bridge listens for.
    #[serde(default)]
    pub events: Vec<String>,
}

/// Host facts the browser uses for path display; the TUI uses them the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostFacts {
    #[serde(default)]
    pub home: Option<String>,
}

// ── stream payloads ──────────────────────────────────────────────────────────

/// How a journal item relates to what the client already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JournalChange {
    /// Adopt these records as the whole contents, discarding what came before.
    Replace,
    /// Older records, ordered before what is held.
    Prepend,
    /// Newer records, ordered after what is held.
    Append,
}

/// One journal stream item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalItem {
    pub change: JournalChange,
    pub records: Vec<HistoryRecord>,
    /// The opening frame's inclusive log cut. Backwards paging must quote this exact
    /// value, so live appends after it cannot shift the page boundaries.
    #[serde(default)]
    pub cursor: Option<u64>,
    /// Whether older records exist before the opening window.
    #[serde(default, rename = "hasMore")]
    pub has_more: bool,
}

impl JournalItem {
    /// A delta carrying no opening-frame metadata.
    pub fn delta(change: JournalChange, records: Vec<HistoryRecord>) -> Self {
        Self { change, records, cursor: None, has_more: false }
    }
}

/// A `SessionHistoryRecord`: one raw event, or one packed run of same-block
/// `assistant/chunk` deltas.
///
/// A packed row's wire `type` is `chunkrow/text-chunks`, `chunkrow/reasoning-chunks`, or
/// `chunkrow/tool-call-chunks`; its `data` is the run payload, not a single chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HistoryRecord {
    Event { event: SessionEvent },
    Chunks { event: SessionEvent },
}

impl HistoryRecord {
    pub fn event(&self) -> &SessionEvent {
        match self {
            HistoryRecord::Event { event } | HistoryRecord::Chunks { event } => event,
        }
    }

    /// Inclusive sequence range this record covers.
    ///
    /// An ordinary record covers `[seq, seq]`. A packed row's `seq` is its *first*
    /// member's, so it covers `[seq, seq + members - 1]` — the range gap detection must
    /// use, not the single seq.
    pub fn seq_range(&self) -> (u64, u64) {
        let event = self.event();
        match self {
            HistoryRecord::Event { .. } => (event.seq, event.seq),
            HistoryRecord::Chunks { .. } => {
                let members = event.member_count().max(1);
                (event.seq, event.seq + members - 1)
            }
        }
    }

    /// The assistant text this record contributes, if any.
    ///
    /// A packed text or reasoning run concatenates its fragments; a tool-call run carries
    /// argument fragments rather than prose and contributes nothing to render here.
    pub fn text(&self) -> Option<String> {
        let event = self.event();
        match self {
            HistoryRecord::Chunks { .. } => {
                let texts = event.data.get("texts")?.as_array()?;
                Some(
                    texts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .concat(),
                )
            }
            HistoryRecord::Event { .. } => event
                .data
                .get("chunk")
                .and_then(|chunk| chunk.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

/// The common shape of both record variants' inner value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub seq: u64,
    #[serde(default)]
    pub time: Option<i64>,
    #[serde(default)]
    pub data: Value,
}

impl SessionEvent {
    /// How many logical `assistant/chunk` deltas a packed row folded in.
    ///
    /// The wire form carries no member count: it is the length of the run arrays —
    /// `texts` for text and reasoning runs, `args` for tool-call runs.
    pub fn member_count(&self) -> u64 {
        let run = self
            .data
            .get("texts")
            .or_else(|| self.data.get("args"))
            .and_then(Value::as_array);
        run.map(|items| items.len() as u64).unwrap_or(1)
    }
}

// ── line codec ───────────────────────────────────────────────────────────────

/// Encode one message as a protocol line, terminator included.
pub fn encode<T: Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    Ok(line)
}

/// Decode one protocol line.
///
/// A malformed line is an error the caller logs and skips: a corrupt frame must not
/// desynchronize the stream.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_round_trips_with_its_tag() {
        let msg = ClientMsg::Call {
            id: 7,
            ns: "session".into(),
            m: "list".into(),
            args: Some(serde_json::json!({})),
        };
        let line = encode(&msg).unwrap();
        assert!(line.ends_with('\n'));
        assert!(line.contains(r#""t":"call""#));
        let back: ClientMsg = decode(line.trim()).unwrap();
        matches!(back, ClientMsg::Call { id: 7, .. });
    }

    #[test]
    fn stream_err_keeps_its_camel_case_tag() {
        let line = r#"{"t":"streamErr","id":3,"code":"internal","message":"boom"}"#;
        let msg: ServerMsg = decode(line).unwrap();
        assert!(matches!(msg, ServerMsg::StreamErr { id: 3, .. }));
    }

    #[test]
    fn packed_row_covers_every_member_seq() {
        // A packed row carries no member count: it is the run array's length.
        let line = r#"{"type":"chunks","event":{"type":"chunkrow/text-chunks","seq":10,
            "time":0,"data":{"turn":1,"step":0,"index":0,"dt":[1,1,1],
            "texts":["a","b","c","d"]}}}"#;
        let record: HistoryRecord = decode(line).unwrap();
        // The packed row's own seq is its first member's; the range must reach the last.
        assert_eq!(record.seq_range(), (10, 13));
        assert_eq!(record.text().as_deref(), Some("abcd"));
    }

    #[test]
    fn a_tool_call_run_is_counted_but_contributes_no_prose() {
        let line = r#"{"type":"chunks","event":{"type":"chunkrow/tool-call-chunks","seq":5,
            "time":0,"data":{"turn":1,"step":0,"index":0,"dt":[1,1],
            "id":"call-1","name":"bash","args":["{\"cmd","\":\"ls","\"}"]}}}"#;
        let record: HistoryRecord = decode(line).unwrap();
        assert_eq!(record.seq_range(), (5, 7));
        assert_eq!(record.text(), None);
    }

    #[test]
    fn ordinary_record_covers_one_seq() {
        let line = r#"{"type":"event","event":{"type":"tool/result","seq":42,"data":{}}}"#;
        let record: HistoryRecord = decode(line).unwrap();
        assert_eq!(record.seq_range(), (42, 42));
    }

    #[test]
    fn waterfall_replies_are_three_distinct_frames() {
        // Delegating must not serialize as a denial: the host fallback depends on it.
        let next = encode(&ClientMsg::Next { id: 1 }).unwrap();
        assert!(next.contains(r#""t":"next""#));
        let reject = encode(&ClientMsg::Reject { id: 1, message: "no".into() }).unwrap();
        assert!(reject.contains(r#""t":"reject""#));
        let answer = encode(&ClientMsg::Answer { id: 1, v: Value::Null }).unwrap();
        assert!(answer.contains(r#""t":"answer""#));
    }
}
