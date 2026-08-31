//! Produced files: the paths a turn actually mutated.
//!
//! Mirrors the web client's rule exactly, because a looser one would claim files changed
//! that did not. Only three first-party tools count, their arguments must parse and
//! validate, and **only successful calls** contribute — a failed write produced nothing.
//!
//! Code-dispatch children do not enter independently; the root `tool/call` events are the
//! definition's input.

use serde_json::Value;

use crate::session::Ledger;
use crate::tool::ToolExchange;

/// The path a supported first-party mutation call writes, if it is one.
///
/// Returns `None` for an unsupported tool, malformed arguments, or a call whose arguments
/// describe no actual change.
pub fn mutation_path(name: &str, arguments_raw: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments_raw).ok()?;
    if !args.is_object() {
        return None;
    }
    match name {
        // A write is a mutation only when it carries content.
        "write" => args
            .get("content")
            .and_then(Value::as_str)
            .and_then(|_| path_value(args.get("file_path"))),
        "edit" => valid_edit(&args).then(|| path_value(args.get("file_path")))?,
        "str_replace_editor" => editor_mutation_path(&args),
        _ => None,
    }
}

/// An edit must replace something with something different.
///
/// An edit whose `old_string` equals its `new_string` changed nothing, so listing its path
/// would claim a file was modified when it was not.
fn valid_edit(args: &Value) -> bool {
    let old = args.get("old_string").and_then(Value::as_str);
    let new = args.get("new_string").and_then(Value::as_str);
    match (old, new) {
        (Some(old), Some(new)) => !old.is_empty() && old != new,
        _ => false,
    }
}

/// Only a mutating editor command produces a path.
fn editor_mutation_path(args: &Value) -> Option<String> {
    let path = path_value(args.get("path"))?;
    match args.get("command").and_then(Value::as_str)? {
        "create" | "insert" | "str_replace" => Some(path),
        // `view` and friends read without producing anything.
        _ => None,
    }
}

fn path_value(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

/// Paths produced by the ledger's successful mutation calls, in first-write order.
pub fn produced(ledger: &Ledger) -> Vec<String> {
    from_exchanges(&ledger.tool_exchanges())
}

/// Paths produced by a set of tool exchanges.
pub fn from_exchanges(exchanges: &[ToolExchange]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for exchange in exchanges {
        // A running call has not produced anything yet, and a failed one never will.
        let Some(result) = exchange.result.as_ref() else { continue };
        if result.failed() {
            continue;
        }
        let Some(path) = mutation_path(&exchange.call.name, &exchange.call.arguments_raw) else {
            continue;
        };
        // The same file written twice is one deliverable.
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolCall, ToolResult};

    fn exchange(name: &str, arguments: &str, settled: Option<bool>) -> ToolExchange {
        let call = ToolCall {
            call_id: format!("c-{name}-{arguments:.8}"),
            name: name.into(),
            arguments_raw: arguments.into(),
            turn: 1,
            step: 0,
        };
        let result = settled.map(|ok| ToolResult {
            call_id: call.call_id.clone(),
            text: String::new(),
            is_error: !ok,
            error: None,
            meta: None,
        });
        ToolExchange { call, result }
    }

    #[test]
    fn a_write_with_content_produces_its_path() {
        assert_eq!(
            mutation_path("write", r#"{"file_path":"src/lex.rs","content":"fn main(){}"}"#),
            Some("src/lex.rs".to_string())
        );
        // Without content there is nothing written.
        assert_eq!(mutation_path("write", r#"{"file_path":"src/lex.rs"}"#), None);
    }

    #[test]
    fn a_no_op_edit_produces_nothing() {
        // old == new changed nothing; claiming the file was modified would be wrong.
        assert_eq!(
            mutation_path("edit", r#"{"file_path":"a.rs","old_string":"x","new_string":"x"}"#),
            None
        );
        assert_eq!(
            mutation_path("edit", r#"{"file_path":"a.rs","old_string":"","new_string":"y"}"#),
            None
        );
        assert_eq!(
            mutation_path("edit", r#"{"file_path":"a.rs","old_string":"x","new_string":"y"}"#),
            Some("a.rs".to_string())
        );
    }

    #[test]
    fn only_mutating_editor_commands_count() {
        assert_eq!(
            mutation_path("str_replace_editor", r#"{"path":"a.rs","command":"create"}"#),
            Some("a.rs".to_string())
        );
        // A view reads without producing anything.
        assert_eq!(
            mutation_path("str_replace_editor", r#"{"path":"a.rs","command":"view"}"#),
            None
        );
    }

    #[test]
    fn unsupported_tools_and_malformed_arguments_produce_nothing() {
        assert_eq!(mutation_path("bash", r#"{"command":"rm -rf /"}"#), None);
        // Arguments are the model's raw string and can be truncated.
        assert_eq!(mutation_path("write", r#"{"file_path":"a.rs","conte"#), None);
        assert_eq!(mutation_path("write", "[]"), None);
    }

    #[test]
    fn only_successful_calls_contribute() {
        let exchanges = vec![
            exchange("write", r#"{"file_path":"ok.rs","content":"x"}"#, Some(true)),
            exchange("write", r#"{"file_path":"failed.rs","content":"x"}"#, Some(false)),
            // Still running: it has not produced anything yet.
            exchange("write", r#"{"file_path":"pending.rs","content":"x"}"#, None),
        ];
        assert_eq!(from_exchanges(&exchanges), vec!["ok.rs"]);
    }

    #[test]
    fn the_same_file_written_twice_is_one_deliverable() {
        let exchanges = vec![
            exchange("write", r#"{"file_path":"a.rs","content":"1"}"#, Some(true)),
            exchange("edit", r#"{"file_path":"a.rs","old_string":"1","new_string":"2"}"#, Some(true)),
            exchange("write", r#"{"file_path":"b.rs","content":"1"}"#, Some(true)),
        ];
        assert_eq!(from_exchanges(&exchanges), vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn a_blank_path_is_not_a_deliverable() {
        assert_eq!(mutation_path("write", r#"{"file_path":"   ","content":"x"}"#), None);
    }
}
