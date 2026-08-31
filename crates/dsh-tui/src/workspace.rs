//! The workspace browser's state: applies `workspace.follow` frames to an ordered list.
//!
//! Every generation starts with exactly one baseline, so an increment arriving on a fresh
//! generation is dropped rather than applied to state it was not computed against.

use dsh_tui_proto::Generation;
use serde::Deserialize;

/// One workspace as the host projects it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceView {
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    /// Canonical host directory path.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub title: String,
    /// Sessions accounted to this workspace, in manual order.
    #[serde(default, rename = "sessionIds")]
    pub session_ids: Vec<String>,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
}

/// The complete reconnect baseline.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Baseline {
    #[serde(default)]
    pub items: Vec<WorkspaceView>,
    #[serde(default, rename = "archivedSessionIds")]
    pub archived_session_ids: Vec<String>,
}

/// One frame of the workspace stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    Baseline { value: Baseline },
    Upsert { workspace: WorkspaceView },
    Remove {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    Order {
        #[serde(rename = "workspaceIds")]
        workspace_ids: Vec<String>,
    },
    Archived {
        #[serde(rename = "archivedSessionIds")]
        archived_session_ids: Vec<String>,
    },
}

#[derive(Debug, Default)]
pub struct Workspaces {
    items: Vec<WorkspaceView>,
    archived: Vec<String>,
    generation: Option<Generation>,
}

impl Workspaces {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[WorkspaceView] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn archived(&self) -> &[String] {
        &self.archived
    }

    /// Apply one frame. Returns false when the frame was dropped.
    pub fn apply(&mut self, generation: Generation, frame: Frame) -> bool {
        let fresh = self.generation != Some(generation);
        match frame {
            Frame::Baseline { value } => {
                self.items = value.items;
                self.archived = value.archived_session_ids;
                self.generation = Some(generation);
                true
            }
            // An increment computed against another generation's state cannot be trusted.
            _ if fresh => false,
            Frame::Upsert { workspace } => {
                match self
                    .items
                    .iter_mut()
                    .find(|item| item.workspace_id == workspace.workspace_id)
                {
                    Some(existing) => *existing = workspace,
                    None => self.items.push(workspace),
                }
                true
            }
            Frame::Remove { workspace_id } => {
                self.items.retain(|item| item.workspace_id != workspace_id);
                true
            }
            Frame::Order { workspace_ids } => {
                // Order by the given ids; anything unnamed keeps its relative position at
                // the end, so a stale order frame cannot silently drop a workspace.
                let mut ordered = Vec::with_capacity(self.items.len());
                for id in &workspace_ids {
                    if let Some(index) = self.items.iter().position(|i| &i.workspace_id == id) {
                        ordered.push(self.items.remove(index));
                    }
                }
                ordered.append(&mut self.items);
                self.items = ordered;
                true
            }
            Frame::Archived {
                archived_session_ids,
            } => {
                self.archived = archived_session_ids;
                true
            }
        }
    }

    /// Rows for the browser: each workspace with its live (non-archived) sessions.
    pub fn rows(&self) -> Vec<WorkspaceRow> {
        self.items
            .iter()
            .map(|item| WorkspaceRow {
                workspace_id: item.workspace_id.clone(),
                title: if item.title.is_empty() {
                    item.path.clone()
                } else {
                    item.title.clone()
                },
                path: item.path.clone(),
                session_ids: item
                    .session_ids
                    .iter()
                    .filter(|id| !self.archived.contains(id))
                    .cloned()
                    .collect(),
            })
            .collect()
    }
}

/// One rendered workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub workspace_id: String,
    /// Title, falling back to the path when the workspace has no title.
    pub title: String,
    pub path: String,
    pub session_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str, title: &str, sessions: &[&str]) -> WorkspaceView {
        WorkspaceView {
            workspace_id: id.into(),
            path: format!("/home/acp/{id}"),
            title: title.into(),
            session_ids: sessions.iter().map(|s| s.to_string()).collect(),
            updated_at: String::new(),
        }
    }

    fn baseline(items: Vec<WorkspaceView>, archived: &[&str]) -> Frame {
        Frame::Baseline {
            value: Baseline {
                items,
                archived_session_ids: archived.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    #[test]
    fn a_baseline_establishes_the_list() {
        let mut store = Workspaces::new();
        assert!(store.apply(1, baseline(vec![workspace("w1", "Harness", &["s1"])], &[])));
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.rows()[0].title, "Harness");
    }

    #[test]
    fn an_increment_before_a_baseline_is_dropped() {
        let mut store = Workspaces::new();
        // No baseline yet: this increment was computed against state we do not have.
        assert!(!store.apply(1, Frame::Upsert { workspace: workspace("w1", "x", &[]) }));
        assert!(store.is_empty());
    }

    #[test]
    fn a_new_generation_requires_its_own_baseline() {
        let mut store = Workspaces::new();
        store.apply(1, baseline(vec![workspace("w1", "one", &[])], &[]));
        assert!(!store.apply(2, Frame::Remove { workspace_id: "w1".into() }));
        assert_eq!(store.items().len(), 1);
        assert!(store.apply(2, baseline(vec![], &[])));
        assert!(store.is_empty());
    }

    #[test]
    fn upsert_updates_in_place_and_appends_new() {
        let mut store = Workspaces::new();
        store.apply(1, baseline(vec![workspace("w1", "one", &[])], &[]));
        store.apply(1, Frame::Upsert { workspace: workspace("w1", "renamed", &[]) });
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].title, "renamed");
        store.apply(1, Frame::Upsert { workspace: workspace("w2", "two", &[]) });
        assert_eq!(store.items().len(), 2);
    }

    #[test]
    fn order_reorders_and_keeps_unnamed_workspaces() {
        let mut store = Workspaces::new();
        store.apply(
            1,
            baseline(
                vec![workspace("a", "A", &[]), workspace("b", "B", &[]), workspace("c", "C", &[])],
                &[],
            ),
        );
        // A stale order frame naming only two must not drop the third.
        store.apply(1, Frame::Order { workspace_ids: vec!["c".into(), "a".into()] });
        let ids: Vec<_> = store.items().iter().map(|i| i.workspace_id.clone()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn archived_sessions_disappear_from_their_workspace_row() {
        let mut store = Workspaces::new();
        store.apply(1, baseline(vec![workspace("w1", "one", &["s1", "s2"])], &[]));
        assert_eq!(store.rows()[0].session_ids.len(), 2);
        store.apply(1, Frame::Archived { archived_session_ids: vec!["s2".into()] });
        assert_eq!(store.rows()[0].session_ids, vec!["s1"]);
    }

    #[test]
    fn a_titleless_workspace_falls_back_to_its_path() {
        let mut store = Workspaces::new();
        store.apply(1, baseline(vec![workspace("w1", "", &[])], &[]));
        assert_eq!(store.rows()[0].title, "/home/acp/w1");
    }

    #[test]
    fn frames_parse_from_the_wire_shape() {
        let frame: Frame = serde_json::from_value(serde_json::json!({
            "type": "order", "workspaceIds": ["a", "b"]
        }))
        .expect("order frame");
        assert!(matches!(frame, Frame::Order { .. }));

        let frame: Frame = serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": { "items": [{ "workspaceId": "w", "path": "/p", "title": "t",
                                   "sessionIds": ["s"], "createdAt": "", "updatedAt": "" }],
                       "archivedSessionIds": [] }
        }))
        .expect("baseline frame");
        match frame {
            Frame::Baseline { value } => {
                assert_eq!(value.items[0].session_ids, vec!["s"]);
            }
            _ => panic!("expected a baseline"),
        }
    }
}
