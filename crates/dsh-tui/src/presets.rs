//! Agent presets and permission presets.
//!
//! Two selection surfaces with a shared hazard: an option that exists in the list is not
//! automatically an option you may switch to.
//!
//! - An agent preset carrying `broken` cannot compose a session. It still belongs in the
//!   roster — hiding it turns "this preset is misconfigured" into "this preset is gone" —
//!   but it must not be selectable.
//! - The permission select appends `custom` **exactly while it is current**: it is derived
//!   from knobs matching no preset, not a value you can choose. Offering it would promise
//!   a switch the host has no table entry for.

use serde::Deserialize;
use serde_json::Value;

// ── agent presets ────────────────────────────────────────────────────────────

/// One preset the configured roots supply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentPreset {
    /// Stable identifier, and the label's fallback.
    pub id: String,
    /// Trust of the root it was discovered under: `system` or `user`.
    #[serde(default)]
    pub trust: String,
    /// Whether a session naming no preset composes this one.
    #[serde(default, rename = "isDefault")]
    pub is_default: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Why this preset cannot compose a session; absent when it can.
    #[serde(default)]
    pub broken: Option<String>,
}

impl AgentPreset {
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.id)
    }

    /// Whether a session can actually be composed with this preset.
    pub fn is_selectable(&self) -> bool {
        self.broken.is_none()
    }

    /// The reason it cannot be used, for the row's trailing note.
    pub fn unusable_reason(&self) -> Option<&str> {
        self.broken.as_deref()
    }
}

/// The roster one deployment supplies.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentPresetRoster {
    #[serde(default)]
    pub presets: Vec<AgentPreset>,
    /// Whether this deployment has a root that locally authored presets go to.
    #[serde(default)]
    pub authorable: bool,
}

impl AgentPresetRoster {
    /// Presets that can compose a session.
    pub fn selectable(&self) -> Vec<&AgentPreset> {
        self.presets.iter().filter(|p| p.is_selectable()).collect()
    }

    /// The preset a session naming none composes.
    pub fn default_preset(&self) -> Option<&AgentPreset> {
        self.presets.iter().find(|p| p.is_default)
    }

    /// Whether the roster has anything a session could actually use.
    pub fn has_usable(&self) -> bool {
        self.presets.iter().any(AgentPreset::is_selectable)
    }
}

// ── permission presets ───────────────────────────────────────────────────────

/// One switchable permission value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PresetOption {
    /// The table key, or `custom`.
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// The session's permission select.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PermissionSelect {
    #[serde(default)]
    pub options: Vec<PresetOption>,
    /// The effective current value: a table key, or `custom`.
    #[serde(default, rename = "currentValue")]
    pub current_value: String,
}

/// The derived value that exists only while it is current.
pub const CUSTOM: &str = "custom";

impl PermissionSelect {
    /// Read the select from the projection map.
    ///
    /// Key absence means no permission service is composed, and clients hide the control
    /// entirely rather than showing an empty picker.
    pub fn read(projections: Option<&Value>) -> Option<Self> {
        let value = projections?.get("permissions")?;
        serde_json::from_value(value.clone()).ok()
    }

    /// Options a human may actually switch to.
    ///
    /// `custom` is derived from knobs matching no preset; the host has no table entry to
    /// switch to, so it never appears as a choice even while it is the current value.
    pub fn switchable(&self) -> Vec<&PresetOption> {
        self.options
            .iter()
            .filter(|option| option.value != CUSTOM)
            .collect()
    }

    /// Whether the session's knobs currently match no preset.
    pub fn is_custom(&self) -> bool {
        self.current_value == CUSTOM
    }

    /// The label for the composer chip.
    pub fn current_label(&self) -> &str {
        self.options
            .iter()
            .find(|option| option.value == self.current_value)
            .map(|option| option.name.as_str())
            .unwrap_or(&self.current_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(value: Value) -> AgentPresetRoster {
        serde_json::from_value(value).expect("roster")
    }

    #[test]
    fn a_broken_preset_is_listed_but_not_selectable() {
        let roster = roster(serde_json::json!({
            "authorable": true,
            "presets": [
                { "id": "coding", "trust": "system", "isDefault": true, "name": "Coding" },
                { "id": "broken-one", "trust": "user", "isDefault": false,
                  "broken": "missing tools/bash" }
            ]
        }));
        // Hiding it would turn "misconfigured" into "gone".
        assert_eq!(roster.presets.len(), 2);
        assert_eq!(roster.selectable().len(), 1);
        assert_eq!(
            roster.presets[1].unusable_reason(),
            Some("missing tools/bash")
        );
    }

    #[test]
    fn a_preset_without_a_name_falls_back_to_its_id() {
        let roster = roster(serde_json::json!({
            "presets": [{ "id": "minimal", "trust": "system", "isDefault": false }]
        }));
        assert_eq!(roster.presets[0].label(), "minimal");
    }

    #[test]
    fn the_default_preset_is_findable() {
        let roster = roster(serde_json::json!({
            "presets": [
                { "id": "a", "trust": "system", "isDefault": false },
                { "id": "b", "trust": "system", "isDefault": true }
            ]
        }));
        assert_eq!(roster.default_preset().map(|p| p.id.as_str()), Some("b"));
    }

    #[test]
    fn a_roster_of_only_broken_presets_has_nothing_usable() {
        let roster = roster(serde_json::json!({
            "presets": [{ "id": "a", "trust": "user", "isDefault": true, "broken": "bad" }]
        }));
        assert!(!roster.has_usable());
        // The default is still reported: it explains why sessions fail to compose.
        assert!(roster.default_preset().is_some());
    }

    #[test]
    fn an_absent_permissions_key_hides_the_control() {
        // Key absence means no permission service is composed.
        assert!(PermissionSelect::read(None).is_none());
        assert!(PermissionSelect::read(Some(&serde_json::json!({ "plan": {} }))).is_none());
    }

    #[test]
    fn custom_is_current_only_and_never_a_choice() {
        let select = PermissionSelect::read(Some(&serde_json::json!({ "permissions": {
            "currentValue": "custom",
            "options": [
                { "value": "safe", "name": "Safe" },
                { "value": "yolo", "name": "Yolo" },
                { "value": "custom", "name": "Custom" }
            ]
        } })))
        .expect("select");
        assert!(select.is_custom());
        assert_eq!(select.current_label(), "Custom");
        // The host has no table entry to switch to, so it is not offered.
        let switchable: Vec<_> = select.switchable().iter().map(|o| o.value.clone()).collect();
        assert_eq!(switchable, vec!["safe", "yolo"]);
    }

    #[test]
    fn a_normal_selection_labels_from_its_option() {
        let select = PermissionSelect::read(Some(&serde_json::json!({ "permissions": {
            "currentValue": "safe",
            "options": [{ "value": "safe", "name": "Safe", "description": "Ask first" }]
        } })))
        .expect("select");
        assert!(!select.is_custom());
        assert_eq!(select.current_label(), "Safe");
        assert_eq!(select.switchable().len(), 1);
    }

    #[test]
    fn an_unknown_current_value_still_labels_something() {
        let select = PermissionSelect::read(Some(&serde_json::json!({ "permissions": {
            "currentValue": "from-the-future", "options": []
        } })))
        .expect("select");
        assert_eq!(select.current_label(), "from-the-future");
    }
}
