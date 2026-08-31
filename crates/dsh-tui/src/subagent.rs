//! The subagent catalog: direct children of a session, and why some have no row.
//!
//! Two properties of this contract are easy to misread:
//!
//! - `activity` is sampled at read time and **encodes no durable outcome**. `running` means
//!   the logical record is resident; `inactive` means it exists only in persistence. An
//!   inactive child has not necessarily finished, so labelling it "done" would be a claim
//!   the catalog never made.
//! - A continuable child can still refuse delivery as an ownership conflict, so
//!   `parentAvailable` and an available-looking row are hints, not guarantees.

use serde::Deserialize;

/// One row of the catalog: a child, or a diagnostic explaining a missing child.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    Child(Child),
    Diagnostic(Diagnostic),
}

/// A direct child session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Child {
    pub id: String,
    /// `running` or `inactive`, sampled when the catalog was read.
    pub activity: String,
    /// Whether a direct descendant is itself a subagent.
    #[serde(default, rename = "hasChildren")]
    pub has_children: bool,
    /// `one-shot` or `continuable`.
    pub mode: String,
    /// Durable creation label; always present for a continuable child.
    #[serde(default)]
    pub label: Option<String>,
}

impl Child {
    /// Whether a human can send this child another message.
    ///
    /// Only a continuable child accepts one, and only when the parent is available for
    /// delivery. Even then the child may reject it as an ownership conflict, so this
    /// gates the control rather than promising it will succeed.
    pub fn can_prompt(&self, parent_available: bool) -> bool {
        self.mode == "continuable" && parent_available
    }

    /// The row's display name.
    pub fn title(&self) -> String {
        match self.label.as_deref().filter(|label| !label.is_empty()) {
            Some(label) => label.to_string(),
            // A one-shot child's label is optional; its id is the only honest fallback.
            None => self.id.clone(),
        }
    }

    /// Status word.
    ///
    /// `inactive` says the record is not resident — not that the child finished — so the
    /// word stays descriptive rather than becoming an outcome.
    pub fn status(&self) -> &'static str {
        match self.activity.as_str() {
            "running" => "running",
            "inactive" => "not resident",
            _ => "unknown",
        }
    }
}

/// A candidate that produced no child row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Diagnostic {
    pub id: String,
    /// `corrupt`, `unavailable`, or `unsupported`.
    pub reason: String,
}

impl Diagnostic {
    /// What to show the reader, and whether it is worth retrying.
    pub fn explanation(&self) -> &'static str {
        match self.reason.as_str() {
            // Deliberately undistinguished upstream: missing, malformed, and
            // unrecognized-version descriptors all arrive as `corrupt`.
            "corrupt" => "descriptor unreadable",
            // Transient: the next listing retries it.
            "unavailable" => "temporarily unreadable",
            // Never produced today; the union keeps it so consumers can route on it.
            "unsupported" => "unsupported child",
            _ => "unrecognized diagnostic",
        }
    }

    pub fn is_transient(&self) -> bool {
        self.reason == "unavailable"
    }
}

/// The catalog as `subagent.list` returns it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Delivery-time hint: whether the parent can currently accept a prompt for a child.
    #[serde(default, rename = "parentAvailable")]
    pub parent_available: bool,
}

impl Catalog {
    /// Only the children, in catalog order.
    pub fn children(&self) -> Vec<&Child> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Child(child) => Some(child),
                Entry::Diagnostic(_) => None,
            })
            .collect()
    }

    /// Only the diagnostics.
    pub fn diagnostics(&self) -> Vec<&Diagnostic> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Diagnostic(diagnostic) => Some(diagnostic),
                Entry::Child(_) => None,
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(value: serde_json::Value) -> Catalog {
        serde_json::from_value(value).expect("catalog")
    }

    #[test]
    fn children_and_diagnostics_are_separated() {
        let catalog = catalog(serde_json::json!({
            "parentAvailable": true,
            "entries": [
                { "kind": "child", "id": "c1", "activity": "running",
                  "hasChildren": false, "mode": "continuable", "label": "reviewer" },
                { "kind": "diagnostic", "id": "c2", "reason": "corrupt" },
                { "kind": "child", "id": "c3", "activity": "inactive",
                  "hasChildren": true, "mode": "one-shot" }
            ]
        }));
        assert_eq!(catalog.children().len(), 2);
        assert_eq!(catalog.diagnostics().len(), 1);
        assert_eq!(catalog.children()[0].title(), "reviewer");
    }

    #[test]
    fn an_inactive_child_is_not_called_finished() {
        let child = Child {
            id: "c1".into(),
            activity: "inactive".into(),
            has_children: false,
            mode: "continuable".into(),
            label: Some("reviewer".into()),
        };
        // `inactive` means the record is not resident, not that the child completed.
        assert_eq!(child.status(), "not resident");
    }

    #[test]
    fn only_a_continuable_child_can_be_prompted() {
        let continuable = Child {
            id: "c1".into(),
            activity: "running".into(),
            has_children: false,
            mode: "continuable".into(),
            label: Some("reviewer".into()),
        };
        let one_shot = Child { mode: "one-shot".into(), ..continuable.clone() };
        assert!(continuable.can_prompt(true));
        assert!(!one_shot.can_prompt(true));
        // The parent must also be available for delivery.
        assert!(!continuable.can_prompt(false));
    }

    #[test]
    fn a_labelless_one_shot_child_falls_back_to_its_id() {
        let child = Child {
            id: "child-42".into(),
            activity: "running".into(),
            has_children: false,
            mode: "one-shot".into(),
            label: None,
        };
        assert_eq!(child.title(), "child-42");
    }

    #[test]
    fn diagnostics_separate_the_retryable_from_the_permanent() {
        let corrupt = Diagnostic { id: "a".into(), reason: "corrupt".into() };
        let transient = Diagnostic { id: "b".into(), reason: "unavailable".into() };
        assert!(!corrupt.is_transient());
        assert_eq!(corrupt.explanation(), "descriptor unreadable");
        assert!(transient.is_transient());
        assert_eq!(transient.explanation(), "temporarily unreadable");
    }

    #[test]
    fn the_unproduced_reason_still_routes() {
        // `unsupported` is never produced today but remains in the union.
        let entry = Diagnostic { id: "c".into(), reason: "unsupported".into() };
        assert_eq!(entry.explanation(), "unsupported child");
        // And an entirely new reason does not panic or read as one of the known ones.
        let future = Diagnostic { id: "d".into(), reason: "quarantined".into() };
        assert_eq!(future.explanation(), "unrecognized diagnostic");
        assert!(!future.is_transient());
    }

    #[test]
    fn an_empty_catalog_is_recognized() {
        let catalog = catalog(serde_json::json!({ "entries": [], "parentAvailable": false }));
        assert!(catalog.is_empty());
        assert!(!catalog.parent_available);
    }
}
