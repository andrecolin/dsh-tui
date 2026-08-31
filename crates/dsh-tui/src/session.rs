//! The conversation ledger: applies journal-stream items to an ordered record list.
//!
//! `RemoteJournalStream` publishes only contiguous `replace`, `prepend`, and `append`
//! changes, removes complete duplicates, and rejects gaps, inverted ranges, and partial
//! overlaps. This mirrors those rules on the client so a dropped or reordered batch shows
//! up as a repairable gap instead of a silently wrong transcript.

use dsh_tui_proto::{Generation, HistoryRecord, JournalChange, JournalItem};
use serde_json::Value;

use crate::tool::{self, ToolCall, ToolExchange, ToolResult};

/// What applying a batch did, or why it could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Apply {
    /// The batch extended or replaced the ledger.
    Applied,
    /// Every record was already held. Duplicates are removed, not treated as an error.
    Duplicate,
    /// A run of sequences is missing. The caller repairs it with a tail page.
    Gap { expected: u64, got: u64 },
    /// The batch partially overlaps what is held, so neither keeping nor dropping it is
    /// safe. Repair rather than guess.
    PartialOverlap,
    /// The batch is internally out of order or a range is inverted.
    Malformed,
}

#[derive(Debug, Default)]
pub struct Ledger {
    records: Vec<HistoryRecord>,
    generation: Option<Generation>,
    /// The opening frame's inclusive log cut, quoted verbatim when paging backwards.
    cursor: Option<u64>,
    /// Whether older records exist before what is held.
    has_more: bool,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &[HistoryRecord] {
        &self.records
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn generation(&self) -> Option<Generation> {
        self.generation
    }

    /// The inclusive log cut a backwards page must quote.
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Whether older records exist before what is held.
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Adopt a backwards page: its records precede what is held.
    ///
    /// The page is contiguous with the ledger by contract, so the same prepend rules apply;
    /// `has_more` reflects whether the host says anything older remains.
    pub fn prepend_page(&mut self, records: Vec<HistoryRecord>, has_more: bool) -> Apply {
        self.has_more = has_more;
        if records.is_empty() {
            return Apply::Applied;
        }
        if let Some(reason) = batch_shape_error(&records) {
            return reason;
        }
        self.prepend(&records)
    }

    /// Inclusive sequence span currently held.
    pub fn span(&self) -> Option<(u64, u64)> {
        let first = self.records.first()?.seq_range().0;
        let last = self.records.last()?.seq_range().1;
        Some((first, last))
    }

    /// Apply one journal item.
    ///
    /// A generation change is not a delta: every generation opens with a complete
    /// baseline, so anything but `replace` on a new generation is a gap to repair.
    pub fn apply(&mut self, generation: Generation, item: &JournalItem) -> Apply {
        if let Some(reason) = batch_shape_error(&item.records) {
            return reason;
        }

        let new_generation = self.generation != Some(generation);
        if new_generation && item.change != JournalChange::Replace {
            let expected = self.span().map(|(_, end)| end + 1).unwrap_or(0);
            let got = item.records.first().map(|r| r.seq_range().0).unwrap_or(0);
            return Apply::Gap { expected, got };
        }

        match item.change {
            JournalChange::Replace => {
                self.records = item.records.clone();
                self.generation = Some(generation);
                // The cut travels with the opening frame; a later delta carries none, and
                // overwriting it with `None` would lose the anchor backfill needs.
                if item.cursor.is_some() {
                    self.cursor = item.cursor;
                    self.has_more = item.has_more;
                }
                Apply::Applied
            }
            JournalChange::Append => self.append(&item.records),
            JournalChange::Prepend => self.prepend(&item.records),
        }
    }

    fn append(&mut self, batch: &[HistoryRecord]) -> Apply {
        let Some((_, held_end)) = self.span() else {
            self.records = batch.to_vec();
            return Apply::Applied;
        };
        let Some(first) = batch.first() else {
            return Apply::Applied;
        };
        let (batch_start, batch_end) = (first.seq_range().0, batch.last().unwrap().seq_range().1);

        if batch_end <= held_end {
            return Apply::Duplicate;
        }
        if batch_start <= held_end {
            // Part of the batch is already held and part is not; accepting it would
            // duplicate records and dropping it would lose the tail.
            return Apply::PartialOverlap;
        }
        if batch_start != held_end + 1 {
            return Apply::Gap {
                expected: held_end + 1,
                got: batch_start,
            };
        }
        self.records.extend_from_slice(batch);
        Apply::Applied
    }

    fn prepend(&mut self, batch: &[HistoryRecord]) -> Apply {
        let Some((held_start, _)) = self.span() else {
            self.records = batch.to_vec();
            return Apply::Applied;
        };
        let Some(last) = batch.last() else {
            return Apply::Applied;
        };
        let (batch_start, batch_end) = (batch.first().unwrap().seq_range().0, last.seq_range().1);

        if batch_start >= held_start {
            return Apply::Duplicate;
        }
        if batch_end >= held_start {
            return Apply::PartialOverlap;
        }
        if batch_end + 1 != held_start {
            return Apply::Gap {
                expected: held_start - 1,
                got: batch_end,
            };
        }
        let mut merged = batch.to_vec();
        merged.append(&mut self.records);
        self.records = merged;
        Apply::Applied
    }
}

/// Reject a batch that is internally inconsistent before it can corrupt the ledger.
fn batch_shape_error(records: &[HistoryRecord]) -> Option<Apply> {
    let mut previous_end: Option<u64> = None;
    for record in records {
        let (start, end) = record.seq_range();
        if end < start {
            return Some(Apply::Malformed);
        }
        if let Some(previous) = previous_end {
            if start != previous + 1 {
                return Some(Apply::Malformed);
            }
        }
        previous_end = Some(end);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_tui_proto::SessionEvent;

    fn event(seq: u64) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "assistant/message".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({}),
            },
        }
    }

    fn packed(seq: u64, members: usize) -> HistoryRecord {
        HistoryRecord::Chunks {
            event: SessionEvent {
                kind: "chunkrow/text-chunks".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 0, "index": 0,
                    "dt": vec![1; members.saturating_sub(1)],
                    "texts": vec!["x"; members],
                }),
            },
        }
    }

    fn item(change: JournalChange, records: Vec<HistoryRecord>) -> JournalItem {
        JournalItem::delta(change, records)
    }

    #[test]
    fn a_baseline_then_appends_build_a_transcript() {
        let mut ledger = Ledger::new();
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Replace, vec![event(1), event(2)])),
            Apply::Applied
        );
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![event(3)])),
            Apply::Applied
        );
        assert_eq!(ledger.span(), Some((1, 3)));
    }

    #[test]
    fn a_packed_row_advances_the_span_by_every_member() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(1)]));
        // Four folded members occupy seq 2..=5, so the next append must start at 6.
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![packed(2, 4)])),
            Apply::Applied
        );
        assert_eq!(ledger.span(), Some((1, 5)));
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![event(6)])),
            Apply::Applied
        );
    }

    #[test]
    fn a_missing_run_is_reported_as_a_repairable_gap() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(1)]));
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![event(5)])),
            Apply::Gap { expected: 2, got: 5 }
        );
        // The ledger is unchanged, so the repair page has something consistent to extend.
        assert_eq!(ledger.span(), Some((1, 1)));
    }

    #[test]
    fn a_fully_held_batch_is_dropped_rather_than_duplicated() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(1), event(2), event(3)]));
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![event(2), event(3)])),
            Apply::Duplicate
        );
        assert_eq!(ledger.records().len(), 3);
    }

    #[test]
    fn a_partial_overlap_is_refused_instead_of_guessed() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(1), event(2)]));
        // Keeping this would duplicate seq 2; dropping it would lose seq 3.
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Append, vec![event(2), event(3)])),
            Apply::PartialOverlap
        );
        assert_eq!(ledger.span(), Some((1, 2)));
    }

    #[test]
    fn prepending_older_history_extends_the_front() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(5), event(6)]));
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Prepend, vec![event(3), event(4)])),
            Apply::Applied
        );
        assert_eq!(ledger.span(), Some((3, 6)));
    }

    #[test]
    fn a_new_generation_must_open_with_a_baseline() {
        let mut ledger = Ledger::new();
        ledger.apply(1, &item(JournalChange::Replace, vec![event(1)]));
        // Generation 2 is a fresh carrier: a delta from it cannot be trusted to continue
        // generation 1's transcript.
        assert!(matches!(
            ledger.apply(2, &item(JournalChange::Append, vec![event(2)])),
            Apply::Gap { .. }
        ));
        assert_eq!(
            ledger.apply(2, &item(JournalChange::Replace, vec![event(1), event(2)])),
            Apply::Applied
        );
        assert_eq!(ledger.generation(), Some(2));
    }

    #[test]
    fn a_non_contiguous_batch_is_malformed() {
        let mut ledger = Ledger::new();
        assert_eq!(
            ledger.apply(1, &item(JournalChange::Replace, vec![event(1), event(3)])),
            Apply::Malformed
        );
        assert!(ledger.is_empty());
    }
}

// ── display projection ───────────────────────────────────────────────────────

/// The kinds of row the conversation renders. Mirrors the web client's node vocabulary:
/// `user/message`, `assistant/message`, `assistant/chunk`, `tool/call`, `tool/result`,
/// and the `turn/start` and `turn/end` boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolResult,
    TurnBoundary,
    Other,
}

/// One rendered conversation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub text: String,
    /// Gutter glyph; tool rows use their card's.
    pub glyph: &'static str,
    /// Whether this row reports a failure.
    pub failed: bool,
}

impl Ledger {
    /// Project the ledger into display rows, merging consecutive assistant text so a
    /// streamed answer reads as one paragraph rather than one row per delta.
    pub fn rows(&self) -> Vec<Row> {
        // A completed `assistant/message` carries the whole block set for its step. The
        // streamed fragments reconstruct the same content, so once the durable message
        // lands it supersedes them; rendering both would show every answer twice.
        let settled: Vec<(u64, u64)> = self
            .records
            .iter()
            .filter(|record| record.event().kind == "assistant/message")
            .map(|record| turn_step(record.event()))
            .collect();

        // The same holds for a tool call: its arguments stream as a packed
        // `chunkrow/tool-call-chunks` run and then land whole on the durable `tool/call`,
        // which is the one that carries the call id and the full argument string.
        let dispatched: Vec<&str> = self
            .records
            .iter()
            .filter(|record| record.event().kind == "tool/call")
            .filter_map(|record| record.event().data.get("callId").and_then(Value::as_str))
            .collect();

        let mut rows: Vec<Row> = Vec::new();
        let mut pending: Option<Row> = None;

        for record in &self.records {
            if is_fragment(record) && settled.contains(&turn_step(record.event())) {
                continue;
            }
            if is_superseded_tool_chunk(record, &dispatched) {
                continue;
            }
            for (kind, text, glyph, failed) in record_rows(record) {
                // Assistant prose and reasoning stream in fragments and must read as one
                // paragraph; everything else is already whole.
                let mergeable = is_fragment(record);
                if let Some(open) = pending.as_mut() {
                    if mergeable && open.kind == kind {
                        open.text.push_str(&text);
                        continue;
                    }
                }
                if let Some(open) = pending.take() {
                    if !open.text.trim().is_empty() {
                        rows.push(open);
                    }
                }
                if mergeable {
                    pending = Some(Row { kind, text, glyph, failed });
                } else if !text.trim().is_empty() {
                    rows.push(Row { kind, text, glyph, failed });
                }
            }
        }

        if let Some(open) = pending {
            if !open.text.trim().is_empty() {
                rows.push(open);
            }
        }
        rows
    }
}

/// Whether this record is a tool-call argument run whose durable `tool/call` has landed.
///
/// A packed run carries the delta's `id`, not a `callId`, so it is matched on that. While
/// the call is still streaming there is no `tool/call` yet and the run is all there is.
fn is_superseded_tool_chunk(record: &HistoryRecord, dispatched: &[&str]) -> bool {
    let event = record.event();
    if event.kind != "chunkrow/tool-call-chunks" {
        return false;
    }
    event
        .data
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| dispatched.contains(&id))
}

/// The turn and step one event belongs to.
fn turn_step(event: &dsh_tui_proto::SessionEvent) -> (u64, u64) {
    (
        event.data.get("turn").and_then(Value::as_u64).unwrap_or(0),
        event.data.get("step").and_then(Value::as_u64).unwrap_or(0),
    )
}

/// The display rows one record contributes.
///
/// Most records make one row. A completed `assistant/message` makes two — its reasoning
/// and its prose — because its content holds both kinds of block and running them together
/// would print the model's thinking as part of its answer.
fn record_rows(record: &HistoryRecord) -> Vec<(RowKind, String, &'static str, bool)> {
    let event = record.event();
    if event.kind == "assistant/message" {
        let mut out = Vec::new();
        let reasoning = message_blocks(&event.data, &["reasoning"]);
        if !reasoning.trim().is_empty() {
            out.push((RowKind::Reasoning, reasoning, "·", false));
        }
        let prose = message_blocks(&event.data, &["text"]);
        if !prose.trim().is_empty() {
            out.push((RowKind::Assistant, prose, " ", false));
        }
        return out;
    }
    let kind = row_kind(record);
    let (text, glyph, failed) = row_body(record, kind);
    vec![(kind, text, glyph, failed)]
}

/// Join the text of a message's content blocks whose `type` is one of `wanted`.
fn message_blocks(data: &Value, wanted: &[&str]) -> String {
    let blocks = data
        .get("message")
        .and_then(|message| message.get("content"))
        .or_else(|| data.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = blocks else {
        return extract_text(data).unwrap_or_default();
    };
    blocks
        .iter()
        .filter(|block| {
            block
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| wanted.contains(&kind))
        })
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn is_fragment(record: &HistoryRecord) -> bool {
    matches!(
        record.event().kind.as_str(),
        "assistant/chunk" | "chunkrow/text-chunks" | "chunkrow/reasoning-chunks"
    )
}

fn row_kind(record: &HistoryRecord) -> RowKind {
    let event = record.event();
    match event.kind.as_str() {
        "user/message" => RowKind::User,
        "assistant/message" | "chunkrow/text-chunks" => RowKind::Assistant,
        "chunkrow/reasoning-chunks" => RowKind::Reasoning,
        "tool/call" | "chunkrow/tool-call-chunks" => RowKind::ToolCall,
        "tool/result" => RowKind::ToolResult,
        "turn/start" | "turn/end" => RowKind::TurnBoundary,
        // A live `assistant/chunk` carries its kind inside the chunk. Persistence packs
        // runs into typed `chunkrow/*` rows, but the live stream does not, so reading
        // only the event name would merge reasoning into the answer with no break.
        "assistant/chunk" => match event
            .data
            .get("chunk")
            .and_then(|chunk| chunk.get("type"))
            .and_then(|kind| kind.as_str())
        {
            Some("reasoning-delta") => RowKind::Reasoning,
            Some("tool-call-delta") => RowKind::ToolCall,
            _ => RowKind::Assistant,
        },
        _ => RowKind::Other,
    }
}

/// Text, gutter glyph, and failure flag for one record.
fn row_body(record: &HistoryRecord, kind: RowKind) -> (String, &'static str, bool) {
    let event = record.event();
    match kind {
        RowKind::Assistant | RowKind::Reasoning => (
            record
                .text()
                .or_else(|| extract_text(&event.data))
                .unwrap_or_default(),
            if kind == RowKind::Reasoning { "·" } else { " " },
            false,
        ),
        RowKind::ToolCall => match ToolCall::from_data(&event.data) {
            Some(call) => {
                let summary = call.summary();
                let text = if summary.is_empty() {
                    format!("{}()", call.name)
                } else {
                    format!("{}  {summary}", call.name)
                };
                (text, call.card().glyph(), false)
            }
            None => (
                event
                    .data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string(),
                "⏵",
                false,
            ),
        },
        RowKind::ToolResult => match ToolResult::from_data(&event.data) {
            Some(result) => {
                let body = first_line(&result.text);
                let text = match (&result.error, result.failed()) {
                    (Some(error), _) => format!("{} [{}] {body}", error.name, error.code),
                    (None, true) => format!("failed: {body}"),
                    (None, false) => body,
                };
                (text, "⏷", result.failed())
            }
            None => (extract_text(&event.data).unwrap_or_default(), "⏷", false),
        },
        RowKind::TurnBoundary => (String::new(), " ", false),
        RowKind::User => (extract_text(&event.data).unwrap_or_default(), "›", false),
        RowKind::Other => (extract_text(&event.data).unwrap_or_default(), " ", false),
    }
}

/// Results can be long; the row shows the first line and the card owns the rest.
fn first_line(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.split('\n').next() {
        Some(line) if !line.is_empty() => line.to_string(),
        _ => String::new(),
    }
}

impl Ledger {
    /// Every tool call in the ledger, paired with its result.
    pub fn tool_exchanges(&self) -> Vec<ToolExchange> {
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for record in &self.records {
            let event = record.event();
            match event.kind.as_str() {
                "tool/call" => {
                    if let Some(call) = ToolCall::from_data(&event.data) {
                        calls.push(call);
                    }
                }
                "tool/result" => {
                    if let Some(result) = ToolResult::from_data(&event.data) {
                        results.push(result);
                    }
                }
                _ => {}
            }
        }
        tool::pair(calls, results)
    }
}

/// Pull display text out of an event payload without assuming one exact shape.
///
/// Content arrives as a bare string, a `{ text }` object, or a `ContentBlock[]`; a reader
/// that insists on one of those renders the others as blank.
fn extract_text(data: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match data {
        Value::String(text) => Some(text.clone()),
        Value::Object(_) => {
            if let Some(text) = data.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
            let content = data.get("content").or_else(|| data.get("message"))?;
            match content {
                Value::String(text) => Some(text.clone()),
                Value::Array(blocks) => {
                    let joined: Vec<String> = blocks
                        .iter()
                        .filter_map(|block| {
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect();
                    (!joined.is_empty()).then(|| joined.join(""))
                }
                other => extract_text(other),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod display_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn streamed_assistant_text_reads_as_one_row() {
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(JournalChange::Replace, vec![
                    text_event("user/message", 1, "fix the parser"),
                    packed_text(2, &["Look", "ing ", "at it"]),
                    packed_text(5, &[" now."]),
                ]),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, RowKind::User);
        assert_eq!(rows[0].text, "fix the parser");
        assert_eq!(rows[1].kind, RowKind::Assistant);
        // Six deltas across two packed rows, one paragraph.
        assert_eq!(rows[1].text, "Looking at it now.");
    }

    #[test]
    fn a_tool_call_breaks_the_assistant_paragraph() {
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(JournalChange::Replace, vec![
                    packed_text(1, &["before"]),
                    tool_call(2, "bash"),
                    packed_text(3, &["after"]),
                ]),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].kind, RowKind::ToolCall);
        // The card leads with the argument that says what the call does.
        assert_eq!(rows[1].text, "bash  ls -la");
        assert_eq!(rows[1].glyph, "$");
        assert_eq!(rows[2].text, "after");
    }

    #[test]
    fn complete_messages_never_run_together() {
        // Two whole assistant messages are two rows; concatenating them would read as
        // `first messagesecond message` with no break.
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(
                JournalChange::Replace,
                vec![
                    text_event("assistant/message", 1, "first message"),
                    text_event("assistant/message", 2, "second message"),
                ],
            ),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "first message");
        assert_eq!(rows[1].text, "second message");
    }

    #[test]
    fn a_message_does_not_absorb_the_fragments_after_it() {
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(
                JournalChange::Replace,
                vec![
                    text_event("assistant/message", 1, "whole"),
                    packed_text(2, &["strea", "med"]),
                ],
            ),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "whole");
        assert_eq!(rows[1].text, "streamed");
    }

    #[test]
    fn a_live_chunk_is_classified_by_the_chunk_not_the_event() {
        // The live stream sends `assistant/chunk` for both reasoning and prose; only
        // persistence packs them into typed rows.
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(
                JournalChange::Replace,
                vec![
                    live_chunk(1, "reasoning-delta", "thinking aloud"),
                    live_chunk(2, "reasoning-delta", " some more"),
                    live_chunk(3, "text-delta", "the answer"),
                ],
            ),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, RowKind::Reasoning);
        assert_eq!(rows[0].text, "thinking aloud some more");
        // Without the chunk-level read this would have run onto the reasoning row.
        assert_eq!(rows[1].kind, RowKind::Assistant);
        assert_eq!(rows[1].text, "the answer");
    }

    #[test]
    fn reasoning_does_not_merge_into_the_answer() {
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(JournalChange::Replace, vec![packed_reasoning(1, &["hmm"]), packed_text(2, &["answer"])]),
        );
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, RowKind::Reasoning);
        assert_eq!(rows[1].kind, RowKind::Assistant);
    }

    #[test]
    fn content_blocks_and_bare_strings_both_render() {
        assert_eq!(
            extract_text(&serde_json::json!({ "content": [{ "type": "text", "text": "a" },
                                                          { "type": "text", "text": "b" }] })),
            Some("ab".to_string())
        );
        assert_eq!(
            extract_text(&serde_json::json!({ "content": "plain" })),
            Some("plain".to_string())
        );
        assert_eq!(extract_text(&serde_json::json!({ "other": 1 })), None);
    }

    #[test]
    fn turn_boundaries_do_not_produce_empty_rows() {
        let mut ledger = Ledger::new();
        ledger.apply(
            1,
            &JournalItem::delta(
                JournalChange::Replace,
                vec![
                    HistoryRecord::Event {
                        event: dsh_tui_proto::SessionEvent {
                            kind: "turn/start".into(),
                            seq: 1,
                            time: Some(0),
                            data: serde_json::json!({}),
                        },
                    },
                    packed_text(2, &["hi"]),
                ],
            ),
        );
        assert_eq!(ledger.rows().len(), 1);
    }
}

#[cfg(test)]
mod tests_support {
    use dsh_tui_proto::{HistoryRecord, SessionEvent};

    pub fn text_event(kind: &str, seq: u64, text: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: kind.into(),
                seq,
                time: Some(0),
                data: serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
            },
        }
    }

    /// The real `tool/call` shape: a pairing id and the model's raw argument string.
    pub fn tool_call(seq: u64, name: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "tool/call".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 0, "callId": format!("call-{seq}"),
                    "name": name, "arguments": "{\"command\":\"ls -la\"}"
                }),
            },
        }
    }

    pub fn packed_text(seq: u64, fragments: &[&str]) -> HistoryRecord {
        packed(seq, "chunkrow/text-chunks", fragments)
    }

    pub fn packed_reasoning(seq: u64, fragments: &[&str]) -> HistoryRecord {
        packed(seq, "chunkrow/reasoning-chunks", fragments)
    }

    /// One live delta, as the follow stream delivers it before persistence packs runs.
    pub fn live_chunk(seq: u64, chunk_type: &str, text: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "assistant/chunk".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 0,
                    "chunk": { "type": chunk_type, "index": 0, "text": text }
                }),
            },
        }
    }

    fn packed(seq: u64, kind: &str, fragments: &[&str]) -> HistoryRecord {
        HistoryRecord::Chunks {
            event: SessionEvent {
                kind: kind.into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 0, "index": 0,
                    "dt": vec![1; fragments.len().saturating_sub(1)],
                    "texts": fragments,
                }),
            },
        }
    }
}

#[cfg(test)]
mod tool_row_tests {
    use super::*;
    use dsh_tui_proto::SessionEvent;

    fn event(kind: &str, seq: u64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(0), data },
        }
    }

    fn ledger_with(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    #[test]
    fn a_failed_result_names_its_internal_error() {
        let ledger = ledger_with(vec![
            event("tool/call", 1, serde_json::json!({
                "turn": 1, "step": 0, "callId": "c1", "name": "bash",
                "arguments": r#"{"command":"false"}"#
            })),
            event("tool/result", 2, serde_json::json!({
                "turn": 1, "step": 0,
                "message": { "content": [{ "toolCallId": "c1", "content": [], "isError": false }] },
                "error": { "name": "AbortError", "code": "aborted" }
            })),
        ]);
        let rows = ledger.rows();
        // `isError` is false here; reading only that would call an abort a success.
        assert!(rows[1].failed);
        assert!(rows[1].text.contains("AbortError"));
        assert!(rows[1].text.contains("aborted"));
    }

    #[test]
    fn a_long_result_shows_only_its_first_line_in_the_row() {
        let ledger = ledger_with(vec![
            event("tool/call", 1, serde_json::json!({
                "turn": 1, "step": 0, "callId": "c1", "name": "read_file",
                "arguments": r#"{"path":"src/lex.rs"}"#
            })),
            event("tool/result", 2, serde_json::json!({
                "turn": 1, "step": 0,
                "message": { "content": [{ "toolCallId": "c1",
                    "content": [{ "type": "text", "text": "line one\nline two\nline three" }] }] }
            })),
        ]);
        let rows = ledger.rows();
        assert_eq!(rows[0].text, "read_file  src/lex.rs");
        assert_eq!(rows[0].glyph, "▤");
        assert_eq!(rows[1].text, "line one");
    }

    #[test]
    fn the_ledger_pairs_its_tool_exchanges() {
        let ledger = ledger_with(vec![
            event("tool/call", 1, serde_json::json!({
                "turn": 1, "step": 0, "callId": "c1", "name": "bash", "arguments": "{}"
            })),
            event("tool/call", 2, serde_json::json!({
                "turn": 1, "step": 0, "callId": "c2", "name": "grep", "arguments": "{}"
            })),
            event("tool/result", 3, serde_json::json!({
                "turn": 1, "step": 0,
                "message": { "content": [{ "toolCallId": "c1", "content": [] }] }
            })),
        ]);
        let exchanges = ledger.tool_exchanges();
        assert_eq!(exchanges.len(), 2);
        assert_eq!(exchanges[0].status(), "done");
        // The second call has not settled, so it is still running.
        assert!(exchanges[1].is_running());
    }
}

#[cfg(test)]
mod message_tests {
    use super::tests_support::*;
    use super::*;
    use dsh_tui_proto::SessionEvent;

    fn assistant_message(seq: u64, reasoning: &str, prose: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "assistant/message".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": { "role": "assistant", "content": [
                        { "type": "reasoning", "text": reasoning },
                        { "type": "text", "text": prose }
                    ] }
                }),
            },
        }
    }

    fn chunk_row(kind: &str, seq: u64, turn: u64, step: u64, text: &str) -> HistoryRecord {
        HistoryRecord::Chunks {
            event: SessionEvent {
                kind: kind.into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": turn, "step": step, "index": 0, "dt": [], "texts": [text]
                }),
            },
        }
    }

    fn ledger(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    #[test]
    fn a_message_splits_its_reasoning_from_its_prose() {
        // The content array holds both kinds; joining them would print the model's
        // thinking as the opening of its answer.
        let ledger = ledger(vec![assistant_message(1, "thinking it over.", "The answer.")]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, RowKind::Reasoning);
        assert_eq!(rows[0].text, "thinking it over.");
        assert_eq!(rows[1].kind, RowKind::Assistant);
        assert_eq!(rows[1].text, "The answer.");
    }

    #[test]
    fn a_settled_message_supersedes_the_fragments_that_built_it() {
        // Both are in the log: the streamed rows and the durable message. Rendering both
        // would show every answer twice.
        let ledger = ledger(vec![
            chunk_row("chunkrow/reasoning-chunks", 1, 1, 1, "thinking it over."),
            chunk_row("chunkrow/text-chunks", 2, 1, 1, "The answer."),
            assistant_message(3, "thinking it over.", "The answer."),
        ]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "thinking it over.");
        assert_eq!(rows[1].text, "The answer.");
    }

    #[test]
    fn fragments_still_render_while_the_step_is_unsettled() {
        // Mid-stream there is no message yet, so the fragments are all there is.
        let ledger = ledger(vec![
            chunk_row("chunkrow/reasoning-chunks", 1, 1, 1, "thinking"),
            chunk_row("chunkrow/text-chunks", 2, 1, 1, "partial ans"),
        ]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].text, "partial ans");
    }

    #[test]
    fn a_settled_step_does_not_suppress_another_steps_fragments() {
        let ledger = ledger(vec![
            chunk_row("chunkrow/text-chunks", 1, 1, 1, "first"),
            assistant_message(2, "", "first"),
            // A later step is still streaming and must keep its rows.
            chunk_row("chunkrow/text-chunks", 3, 1, 2, "second"),
        ]);
        let texts: Vec<_> = ledger.rows().into_iter().map(|row| row.text).collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn a_user_message_is_unaffected_by_the_block_split() {
        let ledger = ledger(vec![text_event("user/message", 1, "do the thing")]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, RowKind::User);
        assert_eq!(rows[0].text, "do the thing");
    }
}

#[cfg(test)]
mod tool_chunk_tests {
    use super::*;
    use dsh_tui_proto::SessionEvent;

    fn tool_chunks(seq: u64, id: &str, args: &[&str]) -> HistoryRecord {
        HistoryRecord::Chunks {
            event: SessionEvent {
                kind: "chunkrow/tool-call-chunks".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 1, "index": 0, "dt": [],
                    "id": id, "name": "bash", "args": args
                }),
            },
        }
    }

    fn tool_call(seq: u64, id: &str, command: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "tool/call".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({
                    "turn": 1, "step": 1, "callId": id, "name": "bash",
                    "arguments": serde_json::json!({ "command": command }).to_string()
                }),
            },
        }
    }

    fn ledger(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    #[test]
    fn the_durable_call_supersedes_its_argument_run() {
        // Both are in the log; rendering both shows the call twice, once without its
        // arguments because a packed run carries no `callId`.
        let ledger = ledger(vec![
            tool_chunks(1, "c1", &["{\"comm", "and\":\"ls\"}"]),
            tool_call(3, "c1", "ls -la"),
        ]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "bash  ls -la");
        // The terminal card, not the generic fallback.
        assert_eq!(rows[0].glyph, "$");
    }

    #[test]
    fn an_unsettled_argument_run_still_shows_the_call_starting() {
        // Mid-stream the durable call has not landed, so the run is the only sign the
        // model is calling a tool at all.
        let ledger = ledger(vec![tool_chunks(1, "c1", &["{"])]);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, RowKind::ToolCall);
    }

    #[test]
    fn another_calls_run_is_not_suppressed_by_an_unrelated_call() {
        let ledger = ledger(vec![
            tool_chunks(1, "c1", &["{}"]),
            tool_call(2, "c2", "echo other"),
        ]);
        // `c1` has not landed yet, so its run stays.
        assert_eq!(ledger.rows().len(), 2);
    }
}
