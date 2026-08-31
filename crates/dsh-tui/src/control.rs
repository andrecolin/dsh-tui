//! Live control state: queues, background jobs, and session projections.
//!
//! `session.control` is a **host-wide** snapshot stream keyed by session id, and every
//! generation opens with exactly one complete baseline — queue and job state are transient
//! process-local facts, not durable events, so a reconnect replaces them rather than
//! resuming.
//!
//! Projection updates carry a `seq` watermark. They can arrive out of order, so a lower
//! watermark never overwrites a higher one for the same key.

use std::collections::HashMap;

use dsh_tui_proto::Generation;
use serde::Deserialize;
use serde_json::Value;

/// One pending inbox occurrence.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QueuedItem {
    pub id: String,
    /// `queued`, `steering`, or `context`.
    #[serde(default)]
    pub placement: String,
    /// Prompt-RPC identity, used to retire the matching local submission echo.
    #[serde(default, rename = "rpcId")]
    pub rpc_id: Option<String>,
}

/// A background job row.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    /// `running`, `stopping`, `completed`, `killed`, or `failed`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default, rename = "startedAt")]
    pub started_at: i64,
    #[serde(default, rename = "finishedAt")]
    pub finished_at: Option<i64>,
}

impl Job {
    /// Whether the job is still doing something.
    pub fn is_live(&self) -> bool {
        matches!(self.status.as_str(), "running" | "stopping")
    }

    /// Whether the job ended badly.
    pub fn failed(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "killed")
    }
}

/// The control stream's frames.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    Baseline {
        value: Baseline,
    },
    Queue {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        items: Vec<QueuedItem>,
    },
    Jobs {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        jobs: Vec<Job>,
    },
    Projection {
        #[serde(rename = "sessionId")]
        session_id: String,
        key: String,
        value: Value,
        #[serde(default)]
        seq: u64,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Baseline {
    #[serde(default)]
    pub queues: HashMap<String, Vec<QueuedItem>>,
    #[serde(default)]
    pub jobs: HashMap<String, Vec<Job>>,
    /// Per-session projection maps, as delivered.
    #[serde(default)]
    pub projections: HashMap<String, Value>,
}

#[derive(Debug, Default)]
pub struct Control {
    queues: HashMap<String, Vec<QueuedItem>>,
    jobs: HashMap<String, Vec<Job>>,
    /// session id → key → (value, watermark).
    projections: HashMap<String, HashMap<String, (Value, u64)>>,
    generation: Option<Generation>,
}

impl Control {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one frame. Returns false when the frame was dropped.
    pub fn apply(&mut self, generation: Generation, frame: Frame) -> bool {
        let fresh = self.generation != Some(generation);
        match frame {
            Frame::Baseline { value } => {
                self.queues = value.queues;
                self.jobs = value.jobs;
                self.projections = value
                    .projections
                    .into_iter()
                    .map(|(session, map)| {
                        let entries = map
                            .as_object()
                            .map(|object| {
                                object
                                    .iter()
                                    .map(|(key, value)| (key.clone(), (value.clone(), 0)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (session, entries)
                    })
                    .collect();
                self.generation = Some(generation);
                true
            }
            // Transient state cannot be resumed across a generation without its baseline.
            _ if fresh => false,
            Frame::Queue { session_id, items } => {
                self.queues.insert(session_id, items);
                true
            }
            Frame::Jobs { session_id, jobs } => {
                self.jobs.insert(session_id, jobs);
                true
            }
            Frame::Projection {
                session_id,
                key,
                value,
                seq,
            } => {
                let entries = self.projections.entry(session_id).or_default();
                match entries.get(&key) {
                    // A lower watermark is a late frame; adopting it would move the
                    // projection backwards.
                    Some((_, held)) if *held > seq => false,
                    _ => {
                        entries.insert(key, (value, seq));
                        true
                    }
                }
            }
        }
    }

    /// The projection map for one session, shaped as the chips read it.
    pub fn projections(&self, session_id: &str) -> Value {
        let Some(entries) = self.projections.get(session_id) else {
            return Value::Null;
        };
        Value::Object(
            entries
                .iter()
                .map(|(key, (value, _))| (key.clone(), value.clone()))
                .collect(),
        )
    }

    pub fn jobs(&self, session_id: &str) -> &[Job] {
        self.jobs.get(session_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn queue(&self, session_id: &str) -> &[QueuedItem] {
        self.queues.get(session_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Jobs still doing something, for the session header count.
    pub fn live_jobs(&self, session_id: &str) -> usize {
        self.jobs(session_id).iter().filter(|job| job.is_live()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> Frame {
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "queues": { "s1": [{ "id": "m1", "placement": "queued", "rpcId": "r1" }] },
                "jobs": { "s1": [
                    { "id": "j1", "kind": "bash", "label": "cargo test",
                      "status": "running", "startedAt": 100 }
                ] },
                "projections": { "s1": { "plan": { "active": true, "pending": false } } }
            }
        }))
        .expect("baseline frame")
    }

    #[test]
    fn a_baseline_establishes_every_axis() {
        let mut control = Control::new();
        assert!(control.apply(1, baseline()));
        assert_eq!(control.queue("s1").len(), 1);
        assert_eq!(control.queue("s1")[0].rpc_id.as_deref(), Some("r1"));
        assert_eq!(control.live_jobs("s1"), 1);
        assert_eq!(control.projections("s1")["plan"]["active"], true);
    }

    #[test]
    fn state_is_isolated_per_session() {
        let mut control = Control::new();
        control.apply(1, baseline());
        // The stream is host-wide; another session's jobs must not leak into this one.
        assert_eq!(control.jobs("s2").len(), 0);
        assert_eq!(control.projections("s2"), Value::Null);
    }

    #[test]
    fn an_increment_without_its_generation_baseline_is_dropped() {
        let mut control = Control::new();
        control.apply(1, baseline());
        let frame: Frame = serde_json::from_value(serde_json::json!({
            "type": "jobs", "sessionId": "s1", "jobs": []
        }))
        .unwrap();
        // Generation 2 has not sent its baseline: queue and job state are transient and
        // cannot be carried across a carrier replacement.
        assert!(!control.apply(2, frame));
        assert_eq!(control.live_jobs("s1"), 1);
    }

    #[test]
    fn a_late_projection_never_moves_the_value_backwards() {
        let mut control = Control::new();
        control.apply(1, baseline());
        let newer: Frame = serde_json::from_value(serde_json::json!({
            "type": "projection", "sessionId": "s1", "key": "plan",
            "value": { "active": false, "pending": false }, "seq": 10
        }))
        .unwrap();
        assert!(control.apply(1, newer));
        assert_eq!(control.projections("s1")["plan"]["active"], false);

        let stale: Frame = serde_json::from_value(serde_json::json!({
            "type": "projection", "sessionId": "s1", "key": "plan",
            "value": { "active": true, "pending": false }, "seq": 4
        }))
        .unwrap();
        // Out-of-order delivery must not resurrect the older value.
        assert!(!control.apply(1, stale));
        assert_eq!(control.projections("s1")["plan"]["active"], false);
    }

    #[test]
    fn a_projection_for_a_new_key_is_added() {
        let mut control = Control::new();
        control.apply(1, baseline());
        let goal: Frame = serde_json::from_value(serde_json::json!({
            "type": "projection", "sessionId": "s1", "key": "goal",
            "value": { "objective": "ship", "phase": "active",
                       "maxGoalRounds": 3, "roundsStarted": 1 },
            "seq": 2
        }))
        .unwrap();
        assert!(control.apply(1, goal));
        let projections = control.projections("s1");
        assert_eq!(projections["goal"]["objective"], "ship");
        // The existing key survives the addition.
        assert_eq!(projections["plan"]["active"], true);
    }

    #[test]
    fn job_states_separate_live_from_finished_and_failed() {
        let mut control = Control::new();
        control.apply(1, baseline());
        let jobs: Frame = serde_json::from_value(serde_json::json!({
            "type": "jobs", "sessionId": "s1", "jobs": [
                { "id": "j1", "status": "running", "startedAt": 1 },
                { "id": "j2", "status": "stopping", "startedAt": 1 },
                { "id": "j3", "status": "completed", "startedAt": 1, "finishedAt": 9 },
                { "id": "j4", "status": "failed", "startedAt": 1, "finishedAt": 9 },
                { "id": "j5", "status": "killed", "startedAt": 1, "finishedAt": 9 }
            ]
        }))
        .unwrap();
        control.apply(1, jobs);
        // Stopping still counts as live: it has not finished yet.
        assert_eq!(control.live_jobs("s1"), 2);
        let failed = control.jobs("s1").iter().filter(|job| job.failed()).count();
        assert_eq!(failed, 2);
    }
}
