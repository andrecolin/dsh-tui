//! Workflow runs: folding the durable run record out of the session log.
//!
//! Four events carry it — `tool-workflow/run-start`, `agent-start`, `agent-end`, `run-end`.
//! Members pair on `(runId, seq)`. Note the two vocabularies differ: a member settles
//! `completed | failed | cancelled`, while a run stops `completed | cancelled | error`.

use std::collections::HashMap;

use crate::session::Ledger;

/// One published workflow member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub seq: u64,
    pub label: String,
    /// Optional phase the member belongs to.
    pub phase: Option<String>,
    pub child_id: String,
    /// `completed`, `failed`, or `cancelled`; `None` while still running.
    pub outcome: Option<String>,
}

impl Member {
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    pub fn failed(&self) -> bool {
        matches!(self.outcome.as_deref(), Some("failed") | Some("cancelled"))
    }

    pub fn status(&self) -> &str {
        self.outcome.as_deref().unwrap_or("running")
    }
}

/// One top-level workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub run_id: String,
    pub name: String,
    pub members: Vec<Member>,
    /// `completed`, `cancelled`, or `error`; `None` while the run is open.
    pub stop_reason: Option<String>,
}

impl Run {
    pub fn is_running(&self) -> bool {
        self.stop_reason.is_none()
    }

    /// A run that stopped badly. The run vocabulary says `error` where a member says
    /// `failed`, so both spellings are checked rather than one being assumed.
    pub fn failed(&self) -> bool {
        matches!(self.stop_reason.as_deref(), Some("error") | Some("cancelled"))
    }

    pub fn status(&self) -> &str {
        self.stop_reason.as_deref().unwrap_or("running")
    }

    /// Members grouped by phase, in first-appearance order, with unphased members last.
    pub fn phases(&self) -> Vec<(Option<String>, Vec<&Member>)> {
        let mut order: Vec<Option<String>> = Vec::new();
        let mut grouped: HashMap<Option<String>, Vec<&Member>> = HashMap::new();
        for member in &self.members {
            let key = member.phase.clone();
            if !order.contains(&key) {
                order.push(key.clone());
            }
            grouped.entry(key).or_default().push(member);
        }
        order.sort_by_key(Option::is_none);
        order
            .into_iter()
            .filter_map(|key| grouped.remove(&key).map(|members| (key, members)))
            .collect()
    }

    pub fn running_members(&self) -> usize {
        self.members.iter().filter(|m| m.is_running()).count()
    }

    pub fn failed_members(&self) -> usize {
        self.members.iter().filter(|m| m.failed()).count()
    }
}

/// Fold every workflow run out of the ledger, in start order.
pub fn runs(ledger: &Ledger) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();

    for record in ledger.records() {
        let event = record.event();
        let data = &event.data;
        let run_id = data.get("runId").and_then(|v| v.as_str()).unwrap_or_default();
        if run_id.is_empty() {
            continue;
        }

        match event.kind.as_str() {
            "tool-workflow/run-start" => runs.push(Run {
                run_id: run_id.to_string(),
                name: data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(run_id)
                    .to_string(),
                members: Vec::new(),
                stop_reason: None,
            }),
            "tool-workflow/agent-start" => {
                // A member for a run we never saw open cannot be placed; the record is
                // incomplete rather than the member being invented under a new run.
                let Some(run) = runs.iter_mut().find(|run| run.run_id == run_id) else {
                    continue;
                };
                run.members.push(Member {
                    seq: data.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
                    label: data
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    phase: data
                        .get("phase")
                        .and_then(|v| v.as_str())
                        .filter(|phase| !phase.is_empty())
                        .map(str::to_string),
                    child_id: data
                        .get("childId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    outcome: None,
                });
            }
            "tool-workflow/agent-end" => {
                let seq = data.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                let Some(run) = runs.iter_mut().find(|run| run.run_id == run_id) else {
                    continue;
                };
                if let Some(member) = run.members.iter_mut().find(|m| m.seq == seq) {
                    member.outcome = data
                        .get("outcome")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            "tool-workflow/run-end" => {
                if let Some(run) = runs.iter_mut().find(|run| run.run_id == run_id) {
                    run.stop_reason = data
                        .get("stopReason")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            _ => {}
        }
    }

    runs
}

#[cfg(test)]
mod tests {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    use super::*;

    fn event(kind: &str, seq: u64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(0), data },
        }
    }

    fn ledger(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    fn full_run() -> Ledger {
        ledger(vec![
            event("tool-workflow/run-start", 1,
                  serde_json::json!({ "runId": "wf1", "name": "review-changes" })),
            event("tool-workflow/agent-start", 2, serde_json::json!({
                "runId": "wf1", "seq": 1, "label": "review:bugs",
                "phase": "Review", "childId": "c1"
            })),
            event("tool-workflow/agent-start", 3, serde_json::json!({
                "runId": "wf1", "seq": 2, "label": "verify:lex.rs",
                "phase": "Verify", "childId": "c2"
            })),
            event("tool-workflow/agent-end", 4,
                  serde_json::json!({ "runId": "wf1", "seq": 1, "outcome": "completed" })),
            event("tool-workflow/agent-end", 5,
                  serde_json::json!({ "runId": "wf1", "seq": 2, "outcome": "failed" })),
            event("tool-workflow/run-end", 6,
                  serde_json::json!({ "runId": "wf1", "stopReason": "error" })),
        ])
    }

    #[test]
    fn a_run_folds_with_its_members() {
        let runs = runs(&full_run());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "review-changes");
        assert_eq!(runs[0].members.len(), 2);
        assert_eq!(runs[0].members[0].status(), "completed");
        assert_eq!(runs[0].failed_members(), 1);
    }

    #[test]
    fn the_run_and_member_failure_vocabularies_both_read_as_failure() {
        let runs = runs(&full_run());
        // A run stops with `error` where a member settles with `failed`.
        assert_eq!(runs[0].status(), "error");
        assert!(runs[0].failed());
        assert!(runs[0].members[1].failed());
    }

    #[test]
    fn an_unsettled_run_and_member_read_as_running() {
        let ledger = ledger(vec![
            event("tool-workflow/run-start", 1,
                  serde_json::json!({ "runId": "wf1", "name": "x" })),
            event("tool-workflow/agent-start", 2, serde_json::json!({
                "runId": "wf1", "seq": 1, "label": "a", "childId": "c1"
            })),
        ]);
        let runs = runs(&ledger);
        assert!(runs[0].is_running());
        assert_eq!(runs[0].running_members(), 1);
        assert_eq!(runs[0].status(), "running");
    }

    #[test]
    fn members_group_by_phase_with_unphased_ones_last() {
        let ledger = ledger(vec![
            event("tool-workflow/run-start", 1, serde_json::json!({ "runId": "w", "name": "n" })),
            event("tool-workflow/agent-start", 2, serde_json::json!({
                "runId": "w", "seq": 1, "label": "loose", "childId": "c0" })),
            event("tool-workflow/agent-start", 3, serde_json::json!({
                "runId": "w", "seq": 2, "label": "a", "phase": "Review", "childId": "c1" })),
            event("tool-workflow/agent-start", 4, serde_json::json!({
                "runId": "w", "seq": 3, "label": "b", "phase": "Review", "childId": "c2" })),
        ]);
        let runs = runs(&ledger);
        let phases = runs[0].phases();
        assert_eq!(phases[0].0.as_deref(), Some("Review"));
        assert_eq!(phases[0].1.len(), 2);
        // Unphased members sort last rather than leading the list.
        assert_eq!(phases[1].0, None);
    }

    #[test]
    fn a_member_for_an_unopened_run_is_not_invented_into_one() {
        let ledger = ledger(vec![event("tool-workflow/agent-start", 1, serde_json::json!({
            "runId": "unknown", "seq": 1, "label": "orphan", "childId": "c1"
        }))]);
        // The record is incomplete; conjuring a run around the member would misreport it.
        assert!(runs(&ledger).is_empty());
    }

    #[test]
    fn concurrent_runs_stay_separate() {
        let ledger = ledger(vec![
            event("tool-workflow/run-start", 1, serde_json::json!({ "runId": "a", "name": "A" })),
            event("tool-workflow/run-start", 2, serde_json::json!({ "runId": "b", "name": "B" })),
            event("tool-workflow/agent-start", 3, serde_json::json!({
                "runId": "b", "seq": 1, "label": "in-b", "childId": "c1" })),
            event("tool-workflow/run-end", 4,
                  serde_json::json!({ "runId": "a", "stopReason": "completed" })),
        ]);
        let runs = runs(&ledger);
        assert_eq!(runs.len(), 2);
        assert!(runs[0].members.is_empty());
        assert_eq!(runs[0].status(), "completed");
        assert_eq!(runs[1].members.len(), 1);
        assert!(runs[1].is_running());
    }

    #[test]
    fn a_run_without_a_name_falls_back_to_its_id() {
        let ledger = ledger(vec![event("tool-workflow/run-start", 1,
                                       serde_json::json!({ "runId": "wf-9" }))]);
        assert_eq!(runs(&ledger)[0].name, "wf-9");
    }
}
