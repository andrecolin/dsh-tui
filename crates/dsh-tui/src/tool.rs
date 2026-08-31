//! Tool calls and their results: pairing, argument decoding, and card selection.
//!
//! A `tool/call` carries `{turn, step, callId, name, arguments}` where `arguments` is the
//! raw JSON string **exactly as the model produced it, unparsed** — so it can be malformed,
//! and a card that assumes valid JSON renders nothing for the one call worth looking at.
//!
//! A `tool/result` carries `{turn, step, message, error?, meta?}` and has no `callId` of its
//! own: the pairing id lives at `message.content[0].toolCallId`.

use serde_json::Value;

/// The model's request to invoke one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    /// Exactly what the model emitted, unparsed.
    pub arguments_raw: String,
    pub turn: u64,
    pub step: u64,
}

impl ToolCall {
    pub fn from_data(data: &Value) -> Option<Self> {
        Some(Self {
            call_id: data.get("callId")?.as_str()?.to_string(),
            name: data.get("name")?.as_str()?.to_string(),
            arguments_raw: data
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            turn: data.get("turn").and_then(Value::as_u64).unwrap_or(0),
            step: data.get("step").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// Decode the arguments, or `None` when the model produced malformed JSON.
    pub fn arguments(&self) -> Option<Value> {
        serde_json::from_str(&self.arguments_raw).ok()
    }

    /// A one-line summary of what the call does, drawn from the argument the tool's card
    /// leads with. Falls back to the raw string so a malformed call still shows something.
    pub fn summary(&self) -> String {
        let Some(args) = self.arguments() else {
            let preview: String = self.arguments_raw.chars().take(60).collect();
            return if preview.is_empty() {
                "malformed arguments".to_string()
            } else {
                format!("malformed arguments: {preview}")
            };
        };
        for key in ["command", "path", "file_path", "query", "url", "pattern", "code"] {
            if let Some(value) = args.get(key).and_then(Value::as_str) {
                return summarize(value);
            }
        }
        String::new()
    }

    /// Which built-in card renders this call.
    pub fn card(&self) -> Card {
        Card::for_tool(&self.name)
    }
}

/// One line, capped, so a long argument cannot push the status off the row.
///
/// Shared with the call tree so a root call and a sub-call summarize identically.
pub fn summarize(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    let capped: String = line.chars().take(60).collect();
    if capped.chars().count() < line.chars().count() {
        format!("{capped}…")
    } else {
        capped
    }
}

/// A completed call's result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    /// Flattened text of the result content blocks.
    pub text: String,
    /// The model-facing error flag on the result block.
    pub is_error: bool,
    /// The internal failure identity, when the tool failed rather than returned an error.
    pub error: Option<ToolError>,
    /// Tool-private presentation payload — `dsh-tool-fs` carries its contextual diff here.
    pub meta: Option<Value>,
}

/// Internal failure identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub name: String,
    pub code: String,
}

impl ToolResult {
    pub fn from_data(data: &Value) -> Option<Self> {
        // The pairing id lives inside the message, not on the event.
        let block = data.get("message")?.get("content")?.get(0)?;
        let call_id = block.get("toolCallId")?.as_str()?.to_string();
        let text = block
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let error = data.get("error").and_then(|value| {
            Some(ToolError {
                name: value.get("name")?.as_str()?.to_string(),
                code: value.get("code")?.as_str()?.to_string(),
            })
        });
        Some(Self {
            call_id,
            text,
            is_error: block.get("isError").and_then(Value::as_bool).unwrap_or(false),
            error,
            meta: data.get("meta").cloned(),
        })
    }

    /// Whether anything went wrong, by either signal.
    ///
    /// `isError` is the model-facing outcome and `error` the internal failure identity; a
    /// card that reads only one of them calls half the failures a success.
    pub fn failed(&self) -> bool {
        self.is_error || self.error.is_some()
    }
}

/// The built-in card kinds, matching `ui-primitives`' output cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Terminal,
    Read,
    Diff,
    Search,
    Web,
    Generic,
}

impl Card {
    /// Choose a card from the tool name.
    pub fn for_tool(name: &str) -> Self {
        let name = name.to_lowercase();
        if name.contains("bash") || name.contains("terminal") || name.contains("shell") {
            Card::Terminal
        } else if name.contains("edit") || name.contains("write") || name.contains("patch") {
            Card::Diff
        } else if name.contains("read") || name.contains("cat") {
            Card::Read
        } else if name.contains("web") || name.contains("fetch") {
            Card::Web
        } else if name.contains("search") || name.contains("grep") || name.contains("glob") {
            Card::Search
        } else {
            Card::Generic
        }
    }

    /// The gutter glyph the card renders with.
    pub fn glyph(self) -> &'static str {
        match self {
            Card::Terminal => "$",
            Card::Read => "▤",
            Card::Diff => "±",
            Card::Search => "⌕",
            Card::Web => "⟐",
            Card::Generic => "⏵",
        }
    }
}

/// One call paired with its result, or still running.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExchange {
    pub call: ToolCall,
    pub result: Option<ToolResult>,
}

impl ToolExchange {
    /// Still running: the call was appended and no result has arrived.
    pub fn is_running(&self) -> bool {
        self.result.is_none()
    }

    pub fn failed(&self) -> bool {
        self.result.as_ref().is_some_and(ToolResult::failed)
    }

    /// Whether the tool attached a presentation payload — a diff, for instance.
    pub fn has_meta(&self) -> bool {
        self.result
            .as_ref()
            .and_then(|r| r.meta.as_ref())
            .is_some_and(|meta| !meta.is_null())
    }

    /// Status word for the card header.
    pub fn status(&self) -> &'static str {
        if self.is_running() {
            "running"
        } else if self.failed() {
            "failed"
        } else {
            "done"
        }
    }
}

/// Pair calls with their results by call id, preserving call order.
///
/// A result whose call is not present is dropped rather than shown alone: without its call
/// there is no tool name, no arguments, and nothing a card could truthfully render.
pub fn pair(calls: Vec<ToolCall>, results: Vec<ToolResult>) -> Vec<ToolExchange> {
    calls
        .into_iter()
        .map(|call| {
            let result = results.iter().find(|r| r.call_id == call.call_id).cloned();
            ToolExchange { call, result }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_data(call_id: &str, name: &str, arguments: &str) -> Value {
        serde_json::json!({
            "turn": 1, "step": 0, "callId": call_id, "name": name, "arguments": arguments
        })
    }

    fn result_data(call_id: &str, text: &str, is_error: bool) -> Value {
        serde_json::json!({
            "turn": 1, "step": 0,
            "message": { "role": "user", "content": [{
                "type": "tool-result", "toolCallId": call_id,
                "content": [{ "type": "text", "text": text }],
                "isError": is_error
            }] }
        })
    }

    #[test]
    fn a_call_decodes_its_arguments_and_leads_with_the_useful_one() {
        let call = ToolCall::from_data(&call_data("c1", "bash", r#"{"command":"cargo test"}"#))
            .expect("call");
        assert_eq!(call.summary(), "cargo test");
        assert_eq!(call.card(), Card::Terminal);
    }

    #[test]
    fn a_long_argument_is_capped_to_one_line() {
        let long = "x".repeat(200);
        let call = ToolCall::from_data(&call_data(
            "c1",
            "run_code",
            &serde_json::json!({ "code": format!("line one\n{long}") }).to_string(),
        ))
        .expect("call");
        let summary = call.summary();
        // A long argument must not push the status off the row.
        assert_eq!(summary, "line one");

        let single = ToolCall::from_data(&call_data(
            "c2",
            "bash",
            &serde_json::json!({ "command": long }).to_string(),
        ))
        .expect("call");
        assert!(single.summary().ends_with('…'));
        assert_eq!(single.summary().chars().count(), 61);
    }

    #[test]
    fn malformed_arguments_still_render_something() {
        // `arguments` is exactly what the model emitted, so it can be truncated or invalid.
        let call = ToolCall::from_data(&call_data("c1", "bash", r#"{"command":"cargo te"#))
            .expect("call");
        assert!(call.arguments().is_none());
        assert!(call.summary().starts_with("malformed arguments:"));
        // The card is still chosen from the tool name, which is not in doubt.
        assert_eq!(call.card(), Card::Terminal);
    }

    #[test]
    fn empty_arguments_do_not_claim_to_be_malformed_content() {
        let call = ToolCall::from_data(&call_data("c1", "list", "")).expect("call");
        assert_eq!(call.summary(), "malformed arguments");
    }

    #[test]
    fn a_result_is_paired_through_the_id_inside_its_message() {
        // The event itself has no callId; it lives at message.content[0].toolCallId.
        let result = ToolResult::from_data(&result_data("c1", "182 lines", false)).expect("result");
        assert_eq!(result.call_id, "c1");
        assert_eq!(result.text, "182 lines");
        assert!(!result.failed());
    }

    #[test]
    fn both_failure_signals_count_as_failure() {
        let model_facing = ToolResult::from_data(&result_data("c1", "no such file", true)).unwrap();
        assert!(model_facing.failed());

        let mut internal = result_data("c2", "", false);
        internal["error"] = serde_json::json!({ "name": "AbortError", "code": "aborted" });
        let internal = ToolResult::from_data(&internal).unwrap();
        // Reading only `isError` would call this a success.
        assert!(!internal.is_error);
        assert!(internal.failed());
        assert_eq!(internal.error.unwrap().code, "aborted");
    }

    #[test]
    fn an_unmatched_call_reads_as_running() {
        let calls = vec![ToolCall::from_data(&call_data("c1", "bash", "{}")).unwrap()];
        let exchanges = pair(calls, vec![]);
        assert!(exchanges[0].is_running());
        assert_eq!(exchanges[0].status(), "running");
    }

    #[test]
    fn a_result_without_its_call_is_dropped_rather_than_shown_alone() {
        let calls = vec![ToolCall::from_data(&call_data("c1", "bash", "{}")).unwrap()];
        let results = vec![
            ToolResult::from_data(&result_data("c1", "ok", false)).unwrap(),
            ToolResult::from_data(&result_data("orphan", "ok", false)).unwrap(),
        ];
        let exchanges = pair(calls, results);
        // Without its call there is no tool name and no arguments to render truthfully.
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].status(), "done");
    }

    #[test]
    fn pairing_survives_results_arriving_out_of_order() {
        let calls = vec![
            ToolCall::from_data(&call_data("c1", "bash", "{}")).unwrap(),
            ToolCall::from_data(&call_data("c2", "read_file", "{}")).unwrap(),
        ];
        let results = vec![
            ToolResult::from_data(&result_data("c2", "second", false)).unwrap(),
            ToolResult::from_data(&result_data("c1", "first", false)).unwrap(),
        ];
        let exchanges = pair(calls, results);
        // Call order is preserved regardless of settle order.
        assert_eq!(exchanges[0].result.as_ref().unwrap().text, "first");
        assert_eq!(exchanges[1].result.as_ref().unwrap().text, "second");
    }

    #[test]
    fn a_presentation_payload_is_noticed() {
        let mut data = result_data("c1", "", false);
        data["meta"] = serde_json::json!({ "diff": "@@ -1 +1 @@" });
        let result = ToolResult::from_data(&data).unwrap();
        let calls = vec![ToolCall::from_data(&call_data("c1", "edit_file", "{}")).unwrap()];
        let exchanges = pair(calls, vec![result]);
        assert!(exchanges[0].has_meta());
        assert_eq!(exchanges[0].call.card(), Card::Diff);
    }

    #[test]
    fn cards_are_chosen_by_tool_name() {
        assert_eq!(Card::for_tool("bash"), Card::Terminal);
        assert_eq!(Card::for_tool("run_terminal_command"), Card::Terminal);
        assert_eq!(Card::for_tool("read_file"), Card::Read);
        assert_eq!(Card::for_tool("edit_file"), Card::Diff);
        assert_eq!(Card::for_tool("web_search"), Card::Web);
        assert_eq!(Card::for_tool("grep"), Card::Search);
        assert_eq!(Card::for_tool("todo_write"), Card::Diff);
        assert_eq!(Card::for_tool("something_new"), Card::Generic);
    }
}
