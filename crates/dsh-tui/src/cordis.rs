//! The dynamic Cordis surface: run status for plugins defined at runtime.
//!
//! The status vocabulary is wider than "working / broken" and the distinctions matter:
//! `awaiting-approval` and `rejected` are decisions, not faults, and `waiting` means the
//! fiber was created and is blocked on services that have not arrived. Collapsing those
//! into "failed" would send someone debugging a plugin that is only waiting for a peer.

use serde::Deserialize;

/// Lifecycle of one activation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    AwaitingApproval,
    StartingHost,
    ClientPending,
    Running,
    /// Created, but blocked on services that have not arrived.
    Waiting,
    Rejected,
    Failed,
    Cancelled,
    Stopped,
    /// A status this build does not know.
    Unrecognized,
}

impl RunStatus {
    pub fn parse(value: &str) -> Self {
        match value {
            "awaiting-approval" => RunStatus::AwaitingApproval,
            "starting-host" => RunStatus::StartingHost,
            "client-pending" => RunStatus::ClientPending,
            "running" => RunStatus::Running,
            "waiting" => RunStatus::Waiting,
            "rejected" => RunStatus::Rejected,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            "stopped" => RunStatus::Stopped,
            _ => RunStatus::Unrecognized,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RunStatus::AwaitingApproval => "awaiting approval",
            RunStatus::StartingHost => "starting",
            RunStatus::ClientPending => "client pending",
            RunStatus::Running => "running",
            RunStatus::Waiting => "waiting on services",
            RunStatus::Rejected => "rejected",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Stopped => "stopped",
            RunStatus::Unrecognized => "unrecognized status",
        }
    }

    /// Whether something went wrong, as opposed to a decision or a wait.
    ///
    /// `rejected` is a human declining, and `waiting` is a healthy fiber blocked on a peer;
    /// neither is a fault to chase.
    pub fn is_fault(self) -> bool {
        matches!(self, RunStatus::Failed)
    }

    /// Whether the attempt is still in motion.
    pub fn is_settling(self) -> bool {
        matches!(
            self,
            RunStatus::AwaitingApproval
                | RunStatus::StartingHost
                | RunStatus::ClientPending
                | RunStatus::Waiting
        )
    }
}

/// One platform half within an activation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HalfState {
    #[serde(default)]
    pub status: String,
    /// Services a created fiber still needs.
    #[serde(default, rename = "waitingFor")]
    pub waiting_for: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl HalfState {
    /// What this half is blocked on, if anything.
    pub fn blocked_on(&self) -> Option<String> {
        (!self.waiting_for.is_empty()).then(|| self.waiting_for.join(", "))
    }
}

/// A structured failure tied to one activation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Diagnostic {
    /// The stage that failed.
    pub phase: String,
    pub message: String,
    #[serde(default, rename = "pluginId")]
    pub plugin_id: String,
}

impl Diagnostic {
    /// A one-line summary naming the stage, since the same message means different things
    /// at load time and at render time.
    pub fn summary(&self) -> String {
        format!("{}: {}", self.phase, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_status_parses() {
        for (wire, expected) in [
            ("awaiting-approval", RunStatus::AwaitingApproval),
            ("starting-host", RunStatus::StartingHost),
            ("client-pending", RunStatus::ClientPending),
            ("running", RunStatus::Running),
            ("waiting", RunStatus::Waiting),
            ("rejected", RunStatus::Rejected),
            ("failed", RunStatus::Failed),
            ("cancelled", RunStatus::Cancelled),
            ("stopped", RunStatus::Stopped),
        ] {
            assert_eq!(RunStatus::parse(wire), expected, "{wire}");
            assert!(!RunStatus::parse(wire).label().is_empty());
        }
    }

    #[test]
    fn decisions_and_waits_are_not_faults() {
        // Someone declining is not a bug to chase.
        assert!(!RunStatus::Rejected.is_fault());
        // Neither is a healthy fiber blocked on a peer that has not arrived.
        assert!(!RunStatus::Waiting.is_fault());
        assert!(!RunStatus::Cancelled.is_fault());
        assert!(RunStatus::Failed.is_fault());
    }

    #[test]
    fn settling_states_are_distinguished_from_terminal_ones() {
        assert!(RunStatus::AwaitingApproval.is_settling());
        assert!(RunStatus::Waiting.is_settling());
        assert!(!RunStatus::Running.is_settling());
        assert!(!RunStatus::Failed.is_settling());
    }

    #[test]
    fn an_unknown_status_is_shown_not_guessed() {
        let status = RunStatus::parse("quiesced");
        assert_eq!(status, RunStatus::Unrecognized);
        assert_eq!(status.label(), "unrecognized status");
        // An upstream addition is not this build's fault to raise an alarm over.
        assert!(!status.is_fault());
    }

    #[test]
    fn a_half_reports_what_it_waits_for() {
        let half: HalfState = serde_json::from_value(serde_json::json!({
            "status": "waiting", "waitingFor": ["ctx.tools", "ctx.llm"]
        }))
        .unwrap();
        assert_eq!(half.blocked_on().as_deref(), Some("ctx.tools, ctx.llm"));

        let running: HalfState = serde_json::from_value(serde_json::json!({
            "status": "running", "waitingFor": []
        }))
        .unwrap();
        assert_eq!(running.blocked_on(), None);
    }

    #[test]
    fn a_diagnostic_names_the_stage_that_failed() {
        let diagnostic: Diagnostic = serde_json::from_value(serde_json::json!({
            "phase": "client-apply", "message": "missing export",
            "pluginId": "p1", "packageId": "k1", "pluginRunId": "r1"
        }))
        .unwrap();
        // The same message means different things at load time and at render time.
        assert_eq!(diagnostic.summary(), "client-apply: missing export");
    }
}
