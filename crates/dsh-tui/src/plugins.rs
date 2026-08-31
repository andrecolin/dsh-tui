//! The Plugins settings pages: the read-only Loader inventory and its search.
//!
//! `pluginInventory.list` is a point-in-time projection of the Loader tree. `enabled` is
//! *effective* enablement — a plugin under a disabled ancestor group already reads false —
//! and `fiberPhase` is null when an entry has no live root fiber.

use serde::Deserialize;

/// One non-group Loader entry.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InventoryEntry {
    #[serde(rename = "entryId")]
    pub entry_id: String,
    /// Exact module specifier the Loader entry imports.
    #[serde(rename = "moduleName")]
    pub module_name: String,
    /// Effective enablement, including disabled ancestor groups.
    #[serde(default)]
    pub enabled: bool,
    /// Lifecycle state of the entry's root fiber, or null when it has none.
    #[serde(default, rename = "fiberPhase")]
    pub fiber_phase: Option<String>,
}

/// The snapshot `pluginInventory.list` returns.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub entries: Vec<InventoryEntry>,
}

/// How an entry reads to someone scanning the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Running.
    Active,
    /// The Loader tried and failed.
    Failed,
    /// Still coming up.
    Loading,
    /// Shutting down.
    Unloading,
    /// Disabled, and correspondingly has no fiber. Expected, not a problem.
    Disabled,
    /// Enabled but with no live root fiber — the one combination worth noticing.
    Inert,
    /// A lifecycle phase this build does not know. The harness's vocabularies are
    /// merge-extensible, so a new phase is an upstream addition, not a fault.
    Unrecognized,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Active => "active",
            Health::Failed => "failed",
            Health::Loading => "loading",
            Health::Unloading => "unloading",
            Health::Disabled => "disabled",
            Health::Inert => "enabled, no fiber",
            Health::Unrecognized => "unrecognized phase",
        }
    }

    /// Whether this state deserves the reader's attention.
    pub fn is_problem(self) -> bool {
        matches!(self, Health::Failed | Health::Inert)
    }
}

impl InventoryEntry {
    pub fn health(&self) -> Health {
        match (self.enabled, self.fiber_phase.as_deref()) {
            (_, Some("failed")) => Health::Failed,
            (_, Some("active")) => Health::Active,
            (_, Some("loading")) | (_, Some("pending")) => Health::Loading,
            (_, Some("unloading")) => Health::Unloading,
            // A disabled entry having no fiber is the expected pairing.
            (false, _) => Health::Disabled,
            // Enabled with no live root fiber is the combination worth surfacing.
            (true, None) => Health::Inert,
            // Reporting an upstream vocabulary addition as broken would cry wolf; the row
            // still shows the raw phase so it is not silently swallowed either.
            (true, Some(_)) => Health::Unrecognized,
        }
    }

    /// The trailing segment of the module specifier, for a dense list.
    pub fn short_name(&self) -> &str {
        self.module_name
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(&self.module_name)
    }
}

/// The inventory page's state: the snapshot plus its search query.
#[derive(Debug, Default)]
pub struct Inventory {
    entries: Vec<InventoryEntry>,
    pub query: String,
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace(&mut self, snapshot: Snapshot) {
        self.entries = snapshot.entries;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries matching the query, case-insensitively, by module specifier.
    pub fn rows(&self) -> Vec<&InventoryEntry> {
        let query = self.query.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                query.is_empty() || entry.module_name.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// Counts for the summary line: total, active, and how many need attention.
    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            total: self.entries.len(),
            ..Summary::default()
        };
        for entry in &self.entries {
            match entry.health() {
                Health::Active => summary.active += 1,
                Health::Disabled => summary.disabled += 1,
                health if health.is_problem() => summary.problems += 1,
                _ => {}
            }
        }
        summary
    }
}

/// Inventory counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub active: usize,
    pub disabled: usize,
    /// Failed, or enabled with no live fiber.
    pub problems: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(module: &str, enabled: bool, phase: Option<&str>) -> InventoryEntry {
        InventoryEntry {
            entry_id: format!("id-{module}"),
            module_name: module.into(),
            enabled,
            fiber_phase: phase.map(str::to_string),
        }
    }

    fn inventory(entries: Vec<InventoryEntry>) -> Inventory {
        let mut inventory = Inventory::new();
        inventory.replace(Snapshot { entries });
        inventory
    }

    #[test]
    fn a_disabled_entry_without_a_fiber_is_expected_not_a_problem() {
        assert_eq!(entry("a", false, None).health(), Health::Disabled);
        assert!(!Health::Disabled.is_problem());
    }

    #[test]
    fn an_enabled_entry_without_a_fiber_is_the_combination_worth_noticing() {
        assert_eq!(entry("a", true, None).health(), Health::Inert);
        assert!(Health::Inert.is_problem());
        assert_eq!(Health::Inert.label(), "enabled, no fiber");
    }

    #[test]
    fn a_failed_fiber_reads_as_failed_whatever_its_enablement() {
        // Enablement does not soften a failure: the Loader tried and could not.
        assert_eq!(entry("a", true, Some("failed")).health(), Health::Failed);
        assert_eq!(entry("a", false, Some("failed")).health(), Health::Failed);
    }

    #[test]
    fn pending_and_loading_both_read_as_coming_up() {
        assert_eq!(entry("a", true, Some("pending")).health(), Health::Loading);
        assert_eq!(entry("a", true, Some("loading")).health(), Health::Loading);
    }

    #[test]
    fn an_unknown_phase_is_shown_but_not_reported_as_broken() {
        let health = entry("a", true, Some("quiescing")).health();
        assert_eq!(health, Health::Unrecognized);
        // An upstream lifecycle addition is not this build's fault to raise an alarm over.
        assert!(!health.is_problem());
    }

    #[test]
    fn search_filters_on_the_module_specifier() {
        let mut inventory = inventory(vec![
            entry("@deepseek-ai/dsh-tool-bash", true, Some("active")),
            entry("@deepseek-ai/dsh-web-search-exa", true, Some("active")),
            entry("@acp/dsh-tui-bridge", true, Some("active")),
        ]);
        inventory.query = "TOOL".into();
        assert_eq!(inventory.rows().len(), 1);
        inventory.query = "dsh-".into();
        assert_eq!(inventory.rows().len(), 3);
        inventory.query = "  ".into();
        // Whitespace is not a filter.
        assert_eq!(inventory.rows().len(), 3);
    }

    #[test]
    fn the_summary_separates_disabled_from_broken() {
        let inventory = inventory(vec![
            entry("a", true, Some("active")),
            entry("b", true, Some("active")),
            entry("c", false, None),
            entry("d", true, Some("failed")),
            entry("e", true, None),
        ]);
        assert_eq!(
            inventory.summary(),
            Summary { total: 5, active: 2, disabled: 1, problems: 2 }
        );
    }

    #[test]
    fn short_names_trim_the_scope() {
        assert_eq!(entry("@deepseek-ai/dsh-tool-bash", true, None).short_name(), "dsh-tool-bash");
        assert_eq!(entry("plain", true, None).short_name(), "plain");
        // A trailing slash must not produce an empty label.
        assert_eq!(entry("scope/", true, None).short_name(), "scope/");
    }

    #[test]
    fn the_snapshot_parses_from_the_wire_shape() {
        let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
            "entries": [
                { "entryId": "e1", "moduleName": "@deepseek-ai/dsh-tool-bash",
                  "enabled": true, "fiberPhase": "active" },
                { "entryId": "e2", "moduleName": "@deepseek-ai/dsh-goal",
                  "enabled": false, "fiberPhase": null }
            ]
        }))
        .expect("snapshot");
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[1].fiber_phase, None);
        assert_eq!(snapshot.entries[1].health(), Health::Disabled);
    }
}
