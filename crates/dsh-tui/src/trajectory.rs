//! The trajectory: a turn-aware view of the event ledger with a timing overview.
//!
//! Turns are delimited by `turn/start` and `turn/end`. Tool durations come from the gap
//! between a `tool/call` and its paired `tool/result` — the events carry `time`, so timing
//! is read from the log rather than measured by the client, and a replayed session shows
//! the same numbers as a live one.

use crate::session::Ledger;
use crate::tool::{ToolCall, ToolResult};

/// One tool call's observed duration inside a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedTool {
    pub name: String,
    /// Milliseconds between the call and its result; `None` while it is still running.
    pub millis: Option<i64>,
    pub failed: bool,
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// 1-based position in the transcript.
    pub index: usize,
    pub start_seq: u64,
    /// Milliseconds from `turn/start` to `turn/end`; `None` while the turn is open.
    pub millis: Option<i64>,
    /// How many records the turn covers.
    pub events: usize,
    pub tools: Vec<TimedTool>,
}

impl Turn {
    pub fn is_open(&self) -> bool {
        self.millis.is_none()
    }

    /// Tools that failed in this turn.
    pub fn failures(&self) -> usize {
        self.tools.iter().filter(|tool| tool.failed).count()
    }
}

/// Build the turn list from a ledger.
///
/// Records before the first `turn/start` belong to no turn — a resumed session opens
/// mid-transcript — and are counted in a leading turn with index 0 rather than discarded.
pub fn turns(ledger: &Ledger) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut open: Option<(u64, i64)> = None;
    let mut pending_calls: Vec<(String, String, i64)> = Vec::new();

    for record in ledger.records() {
        let event = record.event();
        let time = event.time.unwrap_or(0);

        match event.kind.as_str() {
            "turn/start" => {
                turns.push(Turn {
                    index: turns.len() + 1,
                    start_seq: event.seq,
                    millis: None,
                    events: 0,
                    tools: Vec::new(),
                });
                open = Some((event.seq, time));
                continue;
            }
            "turn/end" => {
                if let (Some((_, started)), Some(turn)) = (open, turns.last_mut()) {
                    turn.millis = Some(time - started);
                }
                open = None;
                continue;
            }
            _ => {}
        }

        // A record arriving before any turn/start still belongs somewhere: a resumed
        // session opens mid-transcript, and dropping its history would hide it.
        if turns.is_empty() {
            turns.push(Turn {
                index: 0,
                start_seq: event.seq,
                millis: None,
                events: 0,
                tools: Vec::new(),
            });
        }

        let Some(turn) = turns.last_mut() else { continue };
        turn.events += 1;

        match event.kind.as_str() {
            "tool/call" => {
                if let Some(call) = ToolCall::from_data(&event.data) {
                    pending_calls.push((call.call_id, call.name.clone(), time));
                    turn.tools.push(TimedTool {
                        name: call.name,
                        millis: None,
                        failed: false,
                    });
                }
            }
            "tool/result" => {
                if let Some(result) = ToolResult::from_data(&event.data) {
                    if let Some(position) = pending_calls
                        .iter()
                        .position(|(call_id, _, _)| call_id == &result.call_id)
                    {
                        let (_, name, started) = pending_calls.remove(position);
                        // The matching entry may live in an earlier turn when a call
                        // spans a boundary, so search backwards rather than assuming.
                        for candidate in turns.iter_mut().rev() {
                            if let Some(entry) = candidate
                                .tools
                                .iter_mut()
                                .find(|tool| tool.name == name && tool.millis.is_none())
                            {
                                entry.millis = Some(time - started);
                                entry.failed = result.failed();
                                break;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    turns
}

/// A proportional bar for the timing overview, `width` cells wide.
///
/// The longest turn fills the bar and the rest scale against it, so the overview compares
/// turns with each other rather than against an absolute scale no terminal can show.
pub fn bar(millis: i64, longest: i64, width: usize) -> String {
    if longest <= 0 || width == 0 {
        return String::new();
    }
    let filled = ((millis.max(0) as f64 / longest as f64) * width as f64).round() as usize;
    // Any non-zero duration keeps at least one cell: a bar that renders empty reads as
    // "no time", which is a different claim from "briefly".
    let filled = if millis > 0 { filled.max(1) } else { filled };
    "█".repeat(filled.min(width))
}

/// Human-readable duration.
pub fn format_millis(millis: Option<i64>) -> String {
    match millis {
        None => "running".to_string(),
        Some(ms) if ms < 1000 => format!("{ms}ms"),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

#[cfg(test)]
mod tests {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    use super::*;

    fn event(kind: &str, seq: u64, time: i64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(time), data },
        }
    }

    fn plain(kind: &str, seq: u64, time: i64) -> HistoryRecord {
        event(kind, seq, time, serde_json::json!({}))
    }

    fn call(seq: u64, time: i64, call_id: &str, name: &str) -> HistoryRecord {
        event("tool/call", seq, time, serde_json::json!({
            "turn": 1, "step": 0, "callId": call_id, "name": name, "arguments": "{}"
        }))
    }

    fn result(seq: u64, time: i64, call_id: &str, is_error: bool) -> HistoryRecord {
        event("tool/result", seq, time, serde_json::json!({
            "turn": 1, "step": 0,
            "message": { "content": [{ "toolCallId": call_id, "content": [], "isError": is_error }] }
        }))
    }

    fn ledger(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    #[test]
    fn turns_are_delimited_and_timed_from_the_log() {
        let ledger = ledger(vec![
            plain("turn/start", 1, 1_000),
            plain("user/message", 2, 1_010),
            plain("turn/end", 3, 3_500),
            plain("turn/start", 4, 4_000),
            plain("turn/end", 5, 4_250),
        ]);
        let turns = turns(&ledger);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].millis, Some(2_500));
        assert_eq!(turns[0].events, 1);
        assert_eq!(turns[1].millis, Some(250));
    }

    #[test]
    fn an_unfinished_turn_reads_as_open() {
        let ledger = ledger(vec![plain("turn/start", 1, 0), plain("user/message", 2, 5)]);
        let turns = turns(&ledger);
        assert!(turns[0].is_open());
        assert_eq!(format_millis(turns[0].millis), "running");
    }

    #[test]
    fn history_before_the_first_turn_is_kept() {
        // A resumed session opens mid-transcript; dropping those records would hide them.
        let ledger = ledger(vec![
            plain("user/message", 1, 0),
            plain("assistant/message", 2, 5),
            plain("turn/start", 3, 10),
            plain("turn/end", 4, 20),
        ]);
        let turns = turns(&ledger);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].index, 0);
        assert_eq!(turns[0].events, 2);
        assert_eq!(turns[1].index, 2);
    }

    #[test]
    fn tool_durations_come_from_the_call_and_result_timestamps() {
        let ledger = ledger(vec![
            plain("turn/start", 1, 0),
            call(2, 100, "c1", "bash"),
            result(3, 1_600, "c1", false),
            plain("turn/end", 4, 1_700),
        ]);
        let turns = turns(&ledger);
        assert_eq!(turns[0].tools.len(), 1);
        assert_eq!(turns[0].tools[0].millis, Some(1_500));
        assert_eq!(format_millis(turns[0].tools[0].millis), "1.5s");
        assert!(!turns[0].tools[0].failed);
    }

    #[test]
    fn a_failed_tool_is_counted_in_its_turn() {
        let ledger = ledger(vec![
            plain("turn/start", 1, 0),
            call(2, 10, "c1", "bash"),
            result(3, 20, "c1", true),
            plain("turn/end", 4, 30),
        ]);
        let turns = turns(&ledger);
        assert_eq!(turns[0].failures(), 1);
    }

    #[test]
    fn an_unsettled_call_stays_running() {
        let ledger = ledger(vec![plain("turn/start", 1, 0), call(2, 10, "c1", "bash")]);
        let turns = turns(&ledger);
        assert_eq!(turns[0].tools[0].millis, None);
        assert!(!turns[0].tools[0].failed);
    }

    #[test]
    fn a_call_settling_in_a_later_turn_is_still_timed() {
        let ledger = ledger(vec![
            plain("turn/start", 1, 0),
            call(2, 100, "c1", "bash"),
            plain("turn/end", 3, 200),
            plain("turn/start", 4, 300),
            result(5, 1_100, "c1", false),
            plain("turn/end", 6, 1_200),
        ]);
        let turns = turns(&ledger);
        // The duration belongs to the turn that issued the call, not the one it settled in.
        assert_eq!(turns[0].tools[0].millis, Some(1_000));
        assert!(turns[1].tools.is_empty());
    }

    #[test]
    fn bars_scale_against_the_longest_turn() {
        assert_eq!(bar(1_000, 1_000, 10).chars().count(), 10);
        assert_eq!(bar(500, 1_000, 10).chars().count(), 5);
        assert_eq!(bar(0, 1_000, 10), "");
        // A brief turn keeps a cell: an empty bar would read as "no time at all".
        assert_eq!(bar(1, 100_000, 10).chars().count(), 1);
        assert_eq!(bar(100, 0, 10), "");
    }

    #[test]
    fn durations_read_in_the_right_unit() {
        assert_eq!(format_millis(Some(0)), "0ms");
        assert_eq!(format_millis(Some(999)), "999ms");
        assert_eq!(format_millis(Some(1_000)), "1.0s");
        assert_eq!(format_millis(Some(59_900)), "59.9s");
        assert_eq!(format_millis(Some(61_000)), "1m01s");
    }
}
