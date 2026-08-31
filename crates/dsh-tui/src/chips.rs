//! The composer chips: plan mode, the current goal, and the model seat.
//!
//! These read session *projections* — folds of the session log delivered with the control
//! snapshot. Two absence rules matter and are easy to get wrong:
//!
//! - A projection key missing means the **capability is not composed** in this deployment,
//!   never that its value is falsey. Rendering an "off" plan chip on a harness without
//!   plan-mode would offer a control that cannot work.
//! - A goal's `activation` is process-local and never persisted, so the durable projection
//!   omits it. An `active` goal in a process that has disarmed it will not auto-continue,
//!   and the chip must not imply otherwise.

use serde::Deserialize;
use serde_json::Value;

// ── plan mode ────────────────────────────────────────────────────────────────

/// The plan projection's wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PlanProjection {
    /// The logged state in force.
    #[serde(default)]
    pub active: bool,
    /// A logged selection has not yet been recorded as in force.
    #[serde(default)]
    pub pending: bool,
}

/// What the plan chip shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanChip {
    /// plan-mode is not composed in this deployment; show nothing at all.
    Unavailable,
    Off,
    On,
    /// A selection is logged but not yet in force.
    Pending,
}

impl PlanChip {
    /// Read the chip state from the projection map.
    pub fn read(projections: Option<&Value>) -> Self {
        let Some(value) = projections.and_then(|map| map.get("plan")) else {
            // Absence is capability absence, not "off".
            return PlanChip::Unavailable;
        };
        let Ok(plan) = serde_json::from_value::<PlanProjection>(value.clone()) else {
            return PlanChip::Unavailable;
        };
        match (plan.active, plan.pending) {
            (_, true) => PlanChip::Pending,
            (true, false) => PlanChip::On,
            (false, false) => PlanChip::Off,
        }
    }

    /// Whether the chip occupies a seat in the composer at all.
    pub fn is_visible(self) -> bool {
        !matches!(self, PlanChip::Unavailable)
    }

    pub fn label(self) -> &'static str {
        match self {
            PlanChip::Unavailable => "",
            PlanChip::Off => "plan off",
            PlanChip::On => "plan on",
            PlanChip::Pending => "plan pending",
        }
    }
}

// ── goal ─────────────────────────────────────────────────────────────────────

/// The durable goal projection.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Goal {
    pub objective: String,
    /// `active`, `paused`, `blocked`, or `complete`.
    pub phase: String,
    #[serde(default, rename = "blockedReason")]
    pub blocked_reason: Option<BlockReason>,
    #[serde(default, rename = "maxGoalRounds")]
    pub max_goal_rounds: u32,
    #[serde(default, rename = "roundsStarted")]
    pub rounds_started: u32,
    /// Process-local continuation eligibility; absent from the durable projection.
    #[serde(default)]
    pub activation: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BlockReason {
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
}

impl Goal {
    pub fn read(projections: Option<&Value>) -> Option<Self> {
        let value = projections?.get("goal")?;
        serde_json::from_value(value.clone()).ok()
    }

    /// The chip's one line.
    pub fn summary(&self) -> String {
        let rounds = format!("{}/{}", self.rounds_started, self.max_goal_rounds);
        match self.phase.as_str() {
            "blocked" => {
                let code = self
                    .blocked_reason
                    .as_ref()
                    .map(|reason| reason.code.as_str())
                    .unwrap_or("blocked");
                format!("{} · blocked ({code}) · {rounds}", self.objective)
            }
            phase => format!("{} · {phase} · {rounds}", self.objective),
        }
    }

    /// Whether this process will automatically continue the goal.
    ///
    /// An active goal in a process that has disarmed it will not continue on its own, and
    /// the chip must not imply it will. Absence means the projection carried no activation,
    /// which is the durable read — not a claim that it is armed.
    pub fn will_continue(&self) -> Option<bool> {
        match self.activation.as_deref()? {
            "armed" => Some(self.phase == "active"),
            _ => Some(false),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.phase == "complete"
    }
}

// ── model selection ──────────────────────────────────────────────────────────

/// One selectable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub model_name: String,
}

impl ModelChoice {
    pub fn label(&self) -> String {
        format!("{} · {}", self.provider_name, self.model_name)
    }
}

/// A provider whose catalog lookup failed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogFailure {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub message: String,
}

/// The flattened model catalog.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub choices: Vec<ModelChoice>,
    /// Providers that failed, kept so one failure does not hide the rest.
    pub failures: Vec<CatalogFailure>,
    /// Routable providers that returned no models at all.
    pub empty_providers: Vec<String>,
    pub default_label: Option<String>,
}

impl Catalog {
    /// Flatten `session.modelCatalog` into a selectable list.
    ///
    /// Provider failures are isolated by contract: one provider failing must not empty the
    /// picker, so failures are collected beside the choices rather than replacing them.
    /// A routable provider with an empty catalog is also reported — silence there reads as
    /// "not configured" when the truth is "configured, listed nothing".
    pub fn read(value: &Value) -> Self {
        let mut catalog = Catalog {
            failures: value
                .get("failures")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| serde_json::from_value(row.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            ..Catalog::default()
        };

        let mut non_empty: Vec<String> = Vec::new();
        if let Some(groups) = value.get("groups").and_then(Value::as_array) {
            for group in groups {
                let provider = group.get("id").and_then(Value::as_str).unwrap_or_default();
                let provider_name = group
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(provider);
                let models = group.get("models").and_then(Value::as_array);
                let Some(models) = models else { continue };
                if !models.is_empty() {
                    non_empty.push(provider.to_string());
                }
                for model in models {
                    let Some(id) = model.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    catalog.choices.push(ModelChoice {
                        provider: provider.to_string(),
                        provider_name: provider_name.to_string(),
                        model: id.to_string(),
                        model_name: model
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty())
                            .unwrap_or(id)
                            .to_string(),
                    });
                }
            }
        }

        if let Some(routable) = value.get("routableProviders").and_then(Value::as_array) {
            for provider in routable.iter().filter_map(Value::as_str) {
                let failed = catalog.failures.iter().any(|f| f.id == provider);
                if !failed && !non_empty.iter().any(|id| id == provider) {
                    catalog.empty_providers.push(provider.to_string());
                }
            }
        }

        catalog.default_label = value.get("default").and_then(|selection| {
            let provider = selection.get("provider")?.as_str()?;
            let model = selection.get("model")?.as_str()?;
            Some(format!("{provider} · {model}"))
        });
        catalog
    }

    /// Choices matching a query, case-insensitively.
    pub fn filter(&self, query: &str) -> Vec<&ModelChoice> {
        let query = query.trim().to_lowercase();
        self.choices
            .iter()
            .filter(|choice| {
                query.is_empty()
                    || choice.label().to_lowercase().contains(&query)
                    || choice.model.to_lowercase().contains(&query)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_plan_key_means_the_capability_is_absent() {
        // Not "off": rendering an off chip would offer a control that cannot work.
        assert_eq!(PlanChip::read(None), PlanChip::Unavailable);
        assert_eq!(
            PlanChip::read(Some(&serde_json::json!({ "goal": {} }))),
            PlanChip::Unavailable
        );
        assert!(!PlanChip::read(None).is_visible());
    }

    #[test]
    fn plan_states_read_from_the_projection() {
        let off = serde_json::json!({ "plan": { "active": false, "pending": false } });
        assert_eq!(PlanChip::read(Some(&off)), PlanChip::Off);
        assert!(PlanChip::read(Some(&off)).is_visible());

        let on = serde_json::json!({ "plan": { "active": true, "pending": false } });
        assert_eq!(PlanChip::read(Some(&on)), PlanChip::On);

        // A pending selection outranks the state in force: it is what changes next.
        let pending = serde_json::json!({ "plan": { "active": true, "pending": true } });
        assert_eq!(PlanChip::read(Some(&pending)), PlanChip::Pending);
    }

    #[test]
    fn a_goal_summarizes_its_phase_and_rounds() {
        let goal = Goal::read(Some(&serde_json::json!({ "goal": {
            "objective": "Ship the TUI", "phase": "active",
            "maxGoalRounds": 20, "roundsStarted": 3
        } })))
        .expect("goal");
        assert_eq!(goal.summary(), "Ship the TUI · active · 3/20");
        assert!(!goal.is_finished());
    }

    #[test]
    fn a_blocked_goal_names_its_reason_code() {
        let goal = Goal::read(Some(&serde_json::json!({ "goal": {
            "objective": "Ship", "phase": "blocked",
            "blockedReason": { "code": "needs-human", "message": "waiting" },
            "maxGoalRounds": 5, "roundsStarted": 5
        } })))
        .expect("goal");
        assert!(goal.summary().contains("blocked (needs-human)"));
    }

    #[test]
    fn a_disarmed_process_will_not_continue_an_active_goal() {
        let armed = Goal::read(Some(&serde_json::json!({ "goal": {
            "objective": "x", "phase": "active", "activation": "armed",
            "maxGoalRounds": 1, "roundsStarted": 0
        } })))
        .unwrap();
        assert_eq!(armed.will_continue(), Some(true));

        let disarmed = Goal::read(Some(&serde_json::json!({ "goal": {
            "objective": "x", "phase": "active", "activation": "disarmed",
            "maxGoalRounds": 1, "roundsStarted": 0
        } })))
        .unwrap();
        // Active but disarmed: it will not continue on its own.
        assert_eq!(disarmed.will_continue(), Some(false));

        let durable = Goal::read(Some(&serde_json::json!({ "goal": {
            "objective": "x", "phase": "active",
            "maxGoalRounds": 1, "roundsStarted": 0
        } })))
        .unwrap();
        // Activation is process-local and absent from the durable projection; claiming
        // "armed" here would be an invention.
        assert_eq!(durable.will_continue(), None);
    }

    fn catalog_value() -> Value {
        serde_json::json!({
            "default": { "provider": "deepseek-official", "model": "deepseek-v4-pro" },
            "routableProviders": ["deepseek-official", "anthropic", "empty-gw"],
            "groups": [
                { "id": "deepseek-official", "name": "DeepSeek", "models": [
                    { "id": "deepseek-v4-pro", "name": "V4 Pro" },
                    { "id": "deepseek-v4" }
                ] },
                { "id": "empty-gw", "name": "Empty Gateway", "models": [] }
            ],
            "failures": [
                { "id": "anthropic", "name": "Anthropic", "message": "401 unauthorized" }
            ]
        })
    }

    #[test]
    fn a_provider_failure_does_not_empty_the_picker() {
        let catalog = Catalog::read(&catalog_value());
        // Anthropic failed; DeepSeek's models must still be selectable.
        assert_eq!(catalog.choices.len(), 2);
        assert_eq!(catalog.failures.len(), 1);
        assert_eq!(catalog.failures[0].message, "401 unauthorized");
    }

    #[test]
    fn a_routable_provider_that_listed_nothing_is_reported() {
        let catalog = Catalog::read(&catalog_value());
        // Silence here would read as "not configured" when it is "listed nothing".
        assert_eq!(catalog.empty_providers, vec!["empty-gw"]);
        // A failed provider is not also reported as empty.
        assert!(!catalog.empty_providers.iter().any(|p| p == "anthropic"));
    }

    #[test]
    fn a_model_without_a_name_falls_back_to_its_id() {
        let catalog = Catalog::read(&catalog_value());
        assert_eq!(catalog.choices[0].label(), "DeepSeek · V4 Pro");
        assert_eq!(catalog.choices[1].label(), "DeepSeek · deepseek-v4");
    }

    #[test]
    fn the_picker_filters_on_label_and_id() {
        let catalog = Catalog::read(&catalog_value());
        assert_eq!(catalog.filter("v4 pro").len(), 1);
        assert_eq!(catalog.filter("deepseek-v4").len(), 2);
        assert_eq!(catalog.filter("").len(), 2);
    }

    #[test]
    fn the_deployment_default_is_reported() {
        let catalog = Catalog::read(&catalog_value());
        assert_eq!(
            catalog.default_label.as_deref(),
            Some("deepseek-official · deepseek-v4-pro")
        );
    }
}
