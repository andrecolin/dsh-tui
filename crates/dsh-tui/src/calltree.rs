//! The whole-call tree: root tool calls and the sub-dispatches nested inside them.
//!
//! A `run_code` program dispatches other tools. Each sub-dispatch logs a
//! `tool/code-dispatch-start` and settles with exactly one `tool/code-dispatch`, paired by
//! `subCallId`, and nests by `parentCallId` — which may itself be another sub-call, so the
//! structure is a tree rather than one level of children.
//!
//! Two differences from a root call matter:
//!
//! - A sub-call's `arguments` are **already JSON-normalized** before dispatch, so unlike a
//!   root call's raw model string they cannot be malformed.
//! - A start is appended when the scheduler actually enters the tool body, so an unpaired
//!   start is genuinely still running; a call abandoned in the queue logs nothing at all.

use serde_json::Value;

use crate::session::Ledger;
use crate::tool::{Card, ToolExchange};

/// One nested dispatch inside a `run_code` program.
#[derive(Debug, Clone, PartialEq)]
pub struct SubCall {
    pub root_call_id: String,
    /// The immediate parent, which may be the root call or another sub-call.
    pub parent_call_id: String,
    pub sub_call_id: String,
    pub name: String,
    /// Normalized before dispatch, so this is a value rather than a raw string.
    pub arguments: Value,
    /// Set once the dispatch settles.
    pub outcome: Option<SubOutcome>,
    /// Milliseconds between start and settlement, from the two events' timestamps.
    pub millis: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubOutcome {
    pub is_error: bool,
    pub text: String,
}

impl SubCall {
    /// A start with no settlement is running: the tool body pipeline was entered.
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    pub fn failed(&self) -> bool {
        self.outcome.as_ref().is_some_and(|outcome| outcome.is_error)
    }

    pub fn status(&self) -> &'static str {
        if self.is_running() {
            "running"
        } else if self.failed() {
            "failed"
        } else {
            "done"
        }
    }

    pub fn card(&self) -> Card {
        Card::for_tool(&self.name)
    }

    /// A one-line summary from the normalized arguments.
    pub fn summary(&self) -> String {
        for key in ["command", "path", "file_path", "query", "url", "pattern", "code"] {
            if let Some(value) = self.arguments.get(key).and_then(Value::as_str) {
                return crate::tool::summarize(value);
            }
        }
        String::new()
    }
}

/// One node of the call tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// A root `tool/call` and its result.
    Root {
        exchange: ToolExchange,
        children: Vec<Node>,
    },
    /// A nested dispatch.
    Sub {
        call: SubCall,
        children: Vec<Node>,
    },
}

impl Node {
    pub fn children(&self) -> &[Node] {
        match self {
            Node::Root { children, .. } | Node::Sub { children, .. } => children,
        }
    }

    /// The call id other nodes attach to.
    pub fn id(&self) -> &str {
        match self {
            Node::Root { exchange, .. } => &exchange.call.call_id,
            Node::Sub { call, .. } => &call.sub_call_id,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Node::Root { exchange, .. } => {
                let summary = exchange.call.summary();
                if summary.is_empty() {
                    exchange.call.name.clone()
                } else {
                    format!("{}  {summary}", exchange.call.name)
                }
            }
            Node::Sub { call, .. } => {
                let summary = call.summary();
                if summary.is_empty() {
                    call.name.clone()
                } else {
                    format!("{}  {summary}", call.name)
                }
            }
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Node::Root { exchange, .. } => exchange.status(),
            Node::Sub { call, .. } => call.status(),
        }
    }

    pub fn failed(&self) -> bool {
        match self {
            Node::Root { exchange, .. } => exchange.failed(),
            Node::Sub { call, .. } => call.failed(),
        }
    }

    pub fn glyph(&self) -> &'static str {
        match self {
            Node::Root { exchange, .. } => exchange.call.card().glyph(),
            Node::Sub { call, .. } => call.card().glyph(),
        }
    }
}

/// One flattened row for rendering, with its indentation depth.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub label: String,
    pub status: &'static str,
    pub failed: bool,
    pub glyph: &'static str,
    /// Whether the parent this node named was not in the loaded window.
    pub detached: bool,
}

/// Read every sub-dispatch out of the ledger, paired by `subCallId`.
pub fn sub_calls(ledger: &Ledger) -> Vec<SubCall> {
    let mut calls: Vec<SubCall> = Vec::new();
    let mut started: Vec<(String, i64)> = Vec::new();

    for record in ledger.records() {
        let event = record.event();
        let data = &event.data;
        let time = event.time.unwrap_or(0);
        let Some(sub_call_id) = data.get("subCallId").and_then(Value::as_str) else {
            continue;
        };

        match event.kind.as_str() {
            "tool/code-dispatch-start" => {
                started.push((sub_call_id.to_string(), time));
                calls.push(SubCall {
                    root_call_id: string_at(data, "rootCallId"),
                    parent_call_id: string_at(data, "parentCallId"),
                    sub_call_id: sub_call_id.to_string(),
                    name: string_at(data, "name"),
                    arguments: data.get("arguments").cloned().unwrap_or(Value::Null),
                    outcome: None,
                    millis: None,
                });
            }
            "tool/code-dispatch" => {
                let Some(call) = calls
                    .iter_mut()
                    .find(|call| call.sub_call_id == sub_call_id)
                else {
                    continue;
                };
                call.outcome = Some(SubOutcome {
                    is_error: data.get("isError").and_then(Value::as_bool).unwrap_or(false),
                    text: content_text(data.get("content")),
                });
                if let Some(index) = started.iter().position(|(id, _)| id == sub_call_id) {
                    let (_, start) = started.remove(index);
                    call.millis = Some(time - start);
                }
            }
            _ => {}
        }
    }

    calls
}

fn string_at(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn content_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Build the call tree for one ledger.
///
/// A sub-call whose parent is not in the loaded window is kept at the top level rather than
/// dropped: with backwards paging the parent may simply be older than what is held, and
/// hiding the work would misreport what the agent did.
pub fn build(ledger: &Ledger) -> Vec<Node> {
    let exchanges = ledger.tool_exchanges();
    let subs = sub_calls(ledger);

    let mut roots: Vec<Node> = exchanges
        .into_iter()
        .map(|exchange| Node::Root { exchange, children: Vec::new() })
        .collect();

    // Attach in log order so a parent sub-call always exists before its own children.
    let mut detached: Vec<SubCall> = Vec::new();
    for call in subs {
        let parent = call.parent_call_id.clone();
        let node = Node::Sub { call, children: Vec::new() };
        if !attach(&mut roots, &parent, node.clone()) {
            if let Node::Sub { call, .. } = node {
                detached.push(call);
            }
        }
    }
    for call in detached {
        roots.push(Node::Sub { call, children: Vec::new() });
    }
    roots
}

/// Attach `node` under the node with id `parent`, depth-first. Returns whether it landed.
fn attach(nodes: &mut [Node], parent: &str, node: Node) -> bool {
    for candidate in nodes.iter_mut() {
        if candidate.id() == parent {
            match candidate {
                Node::Root { children, .. } | Node::Sub { children, .. } => children.push(node),
            }
            return true;
        }
        let landed = match candidate {
            Node::Root { children, .. } | Node::Sub { children, .. } => {
                attach(children, parent, node.clone())
            }
        };
        if landed {
            return true;
        }
    }
    false
}

/// Flatten the tree into indented rows.
pub fn rows(nodes: &[Node]) -> Vec<Row> {
    let mut out = Vec::new();
    flatten(nodes, 0, &mut out, true);
    out
}

fn flatten(nodes: &[Node], depth: usize, out: &mut Vec<Row>, top: bool) {
    for node in nodes {
        // A top-level sub-call named a parent that is not loaded; say so rather than
        // presenting it as if it ran on its own.
        let detached = top && matches!(node, Node::Sub { .. });
        out.push(Row {
            depth,
            label: node.label(),
            status: node.status(),
            failed: node.failed(),
            glyph: node.glyph(),
            detached,
        });
        flatten(node.children(), depth + 1, out, false);
    }
}

#[cfg(test)]
mod tests {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    use super::*;

    fn event(kind: &str, seq: u64, time: i64, data: Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(time), data },
        }
    }

    fn ledger(records: Vec<HistoryRecord>) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.apply(1, &JournalItem::delta(JournalChange::Replace, records));
        ledger
    }

    fn root_call(seq: u64, id: &str) -> HistoryRecord {
        event("tool/call", seq, 0, serde_json::json!({
            "turn": 1, "step": 0, "callId": id, "name": "run_code", "arguments": "{}"
        }))
    }

    fn start(seq: u64, time: i64, parent: &str, sub: &str, name: &str) -> HistoryRecord {
        event("tool/code-dispatch-start", seq, time, serde_json::json!({
            "rootCallId": "root", "parentCallId": parent, "subCallId": sub,
            "name": name, "arguments": { "command": "ls -la" }
        }))
    }

    fn settle(seq: u64, time: i64, sub: &str, is_error: bool) -> HistoryRecord {
        event("tool/code-dispatch", seq, time, serde_json::json!({
            "rootCallId": "root", "parentCallId": "root", "subCallId": sub,
            "name": "bash", "arguments": {}, "isError": is_error,
            "content": [{ "type": "text", "text": "output" }]
        }))
    }

    #[test]
    fn sub_calls_nest_under_their_root() {
        let ledger = ledger(vec![
            root_call(1, "root"),
            start(2, 100, "root", "root:code:1", "bash"),
            settle(3, 400, "root:code:1", false),
        ]);
        let tree = build(&ledger);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children().len(), 1);

        let rows = rows(&tree);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].label, "bash  ls -la");
        assert_eq!(rows[1].status, "done");
    }

    #[test]
    fn nesting_goes_deeper_than_one_level() {
        // parentCallId may name another sub-call, not only the root.
        let ledger = ledger(vec![
            root_call(1, "root"),
            start(2, 0, "root", "root:code:1", "run_code"),
            start(3, 0, "root:code:1", "root:code:2", "bash"),
        ]);
        let rows = rows(&build(&ledger));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn an_unsettled_start_is_running_not_lost() {
        // A start means the tool body pipeline was entered; an abandoned call logs nothing.
        let ledger = ledger(vec![root_call(1, "root"), start(2, 0, "root", "s1", "bash")]);
        let rows = rows(&build(&ledger));
        assert_eq!(rows[1].status, "running");
        assert!(!rows[1].failed);
    }

    #[test]
    fn a_failed_sub_dispatch_is_marked() {
        let ledger = ledger(vec![
            root_call(1, "root"),
            start(2, 0, "root", "s1", "bash"),
            settle(3, 10, "s1", true),
        ]);
        let rows = rows(&build(&ledger));
        assert!(rows[1].failed);
        assert_eq!(rows[1].status, "failed");
    }

    #[test]
    fn timing_comes_from_the_two_events() {
        let ledger = ledger(vec![
            root_call(1, "root"),
            start(2, 100, "root", "s1", "bash"),
            settle(3, 1_350, "s1", false),
        ]);
        let subs = sub_calls(&ledger);
        assert_eq!(subs[0].millis, Some(1_250));
    }

    #[test]
    fn a_sub_call_whose_parent_is_not_loaded_is_kept_and_marked() {
        // With backwards paging the parent may be older than the held window; hiding the
        // work would misreport what the agent did.
        let ledger = ledger(vec![start(1, 0, "older-root", "s1", "bash")]);
        let rows = rows(&build(&ledger));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].detached);
    }

    #[test]
    fn a_root_call_with_no_sub_dispatches_has_no_children() {
        let ledger = ledger(vec![root_call(1, "root")]);
        let rows = rows(&build(&ledger));
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].detached);
    }

    #[test]
    fn normalized_sub_arguments_need_no_parsing() {
        // Unlike a root call's raw model string, these are already a JSON value.
        let ledger = ledger(vec![root_call(1, "root"), start(2, 0, "root", "s1", "bash")]);
        let subs = sub_calls(&ledger);
        assert_eq!(subs[0].summary(), "ls -la");
        assert!(subs[0].arguments.is_object());
    }
}
