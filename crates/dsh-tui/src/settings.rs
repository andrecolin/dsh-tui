//! Settings: reading the serialized schemastery envelope and building path-addressed edits.
//!
//! The web client rehydrates the schema with `new Schema(json)` and lets schemastery drive
//! the form. Rust has no schemastery, so this reads the serialized envelope directly —
//! `type`, `dict`, `inner`, `list`, and `meta` — and projects the fields a terminal form
//! can actually edit.
//!
//! Writes are path-addressed `SettingsPathOpView` ops carrying the view's `revision`, so a
//! stale editor is refused by the host rather than silently overwriting a concurrent change.
//!
//! **Never `settings.replace` from this form.** The view it renders is redacted — every
//! `role('secret')` field was stripped before it reached the wire — so a section rebuilt
//! from it and sent wholesale would delete every secret the wire never returned. Path ops
//! name only the field being edited, which is exactly why they exist. `settings_never_uses_replace`
//! guards this.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One namespace as `settings.describe()` returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct NamespaceView {
    pub ns: String,
    #[serde(default)]
    pub schema: Value,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub base: Option<Value>,
    /// The raw user section. A field present here is user-overridden.
    #[serde(default)]
    pub user: Option<Value>,
    #[serde(default = "applies_live")]
    pub applies: String,
    #[serde(default)]
    pub secrets: Vec<SecretView>,
    /// Send back as `expectedRevision` on a write.
    #[serde(default)]
    pub revision: u64,
}

fn applies_live() -> String {
    "live".to_string()
}

/// A schema-declared secret slot. The value never rides the wire — only whether it is set.
#[derive(Debug, Clone, Deserialize)]
pub struct SecretView {
    pub path: Vec<String>,
    pub set: bool,
}

/// The `settings.describe()` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Describe {
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub has_document: bool,
    #[serde(default)]
    pub namespaces: Vec<NamespaceView>,
}

/// One path-addressed edit, matching `SettingsPathOpView`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum PathOp {
    Set { path: Vec<String>, value: Value },
    Unset { path: Vec<String> },
}

/// How a field is edited in the terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum Editor {
    Text,
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Bool,
    /// A union of constants — rendered as a picker.
    Select(Vec<String>),
    /// A secret slot: set or clear, never display.
    Secret { set: bool },
    /// A shape this build does not edit inline; shown read-only with its type name.
    Unsupported(String),
}

/// One editable row of a settings namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Path from the section root.
    pub path: Vec<String>,
    /// Dotted path, for display.
    pub label: String,
    pub description: String,
    pub editor: Editor,
    pub required: bool,
    pub disabled: bool,
    /// Resolved value (schema defaults → composition base → user layer).
    pub value: Value,
    /// Whether the user layer overrides this field.
    pub overridden: bool,
}

impl Field {
    /// Render the current value as terminal text, never revealing a secret.
    pub fn display_value(&self) -> String {
        match &self.editor {
            Editor::Secret { set } => (if *set { "••••••••" } else { "not set" }).to_string(),
            _ => match &self.value {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => other.to_string(),
            },
        }
    }
}

/// Flatten a namespace into editable fields.
///
/// Hidden nodes are skipped, secret slots are marked so their values never render, and a
/// node this build cannot edit is surfaced read-only rather than silently dropped — a
/// missing row would read as "this setting does not exist".
pub fn fields(view: &NamespaceView) -> Vec<Field> {
    let mut out = Vec::new();
    walk(
        &view.schema,
        &view.value,
        view.user.as_ref(),
        &view.secrets,
        &mut Vec::new(),
        &mut out,
    );
    out
}

fn walk(
    schema: &Value,
    value: &Value,
    user: Option<&Value>,
    secrets: &[SecretView],
    path: &mut Vec<String>,
    out: &mut Vec<Field>,
) {
    let meta = schema.get("meta");
    if meta_flag(meta, "hidden") {
        return;
    }

    let node_type = schema.get("type").and_then(Value::as_str).unwrap_or("any");

    // An object with a `dict` is a group of fields, not a field itself.
    if node_type == "object" {
        if let Some(dict) = schema.get("dict").and_then(Value::as_object) {
            for (key, child) in dict {
                path.push(key.clone());
                walk(
                    child,
                    value.get(key).unwrap_or(&Value::Null),
                    user.and_then(|u| u.get(key)),
                    secrets,
                    path,
                    out,
                );
                path.pop();
            }
            return;
        }
    }

    // `intersect` merges its members into one shape.
    if node_type == "intersect" {
        if let Some(list) = schema.get("list").and_then(Value::as_array) {
            for member in list {
                walk(member, value, user, secrets, path, out);
            }
            return;
        }
    }

    if path.is_empty() {
        return;
    }

    let secret = secrets.iter().find(|slot| slot.path == *path);
    let editor = match secret {
        Some(slot) => Editor::Secret { set: slot.set },
        None => editor_for(node_type, schema),
    };

    out.push(Field {
        label: path.join("."),
        path: path.clone(),
        description: description_of(meta),
        editor,
        required: meta_flag(meta, "required"),
        disabled: meta_flag(meta, "disabled"),
        value: value.clone(),
        overridden: user.is_some_and(|u| !u.is_null()),
    });
}

fn editor_for(node_type: &str, schema: &Value) -> Editor {
    match node_type {
        "string" => Editor::Text,
        "number" | "natural" | "percent" => Editor::Number {
            min: meta_number(schema, "min"),
            max: meta_number(schema, "max"),
            step: meta_number(schema, "step"),
        },
        "boolean" => Editor::Bool,
        "union" => match const_members(schema) {
            Some(options) => Editor::Select(options),
            // A union of shapes rather than constants is not a picker.
            None => Editor::Unsupported("union".to_string()),
        },
        other => Editor::Unsupported(other.to_string()),
    }
}

/// A union is a picker only when every member is a constant.
fn const_members(schema: &Value) -> Option<Vec<String>> {
    let list = schema.get("list")?.as_array()?;
    let mut options = Vec::new();
    for member in list {
        if member.get("type").and_then(Value::as_str)? != "const" {
            return None;
        }
        let value = member.get("value")?;
        options.push(match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        });
    }
    (!options.is_empty()).then_some(options)
}

fn meta_flag(meta: Option<&Value>, key: &str) -> bool {
    meta.and_then(|m| m.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn meta_number(schema: &Value, key: &str) -> Option<f64> {
    schema.get("meta")?.get(key)?.as_f64()
}

/// `description` is a plain string or a locale dictionary.
fn description_of(meta: Option<&Value>) -> String {
    let Some(description) = meta.and_then(|m| m.get("description")) else {
        return String::new();
    };
    match description {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("en")
            .or_else(|| map.values().next())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Build the write for one field edit.
///
/// Clearing a field `unset`s it so the resolved value falls back to the composition base or
/// schema default, rather than pinning an empty string as a user override.
pub fn edit_op(field: &Field, input: &str) -> Result<PathOp, String> {
    if input.is_empty() {
        return Ok(PathOp::Unset {
            path: field.path.clone(),
        });
    }
    let value = match &field.editor {
        Editor::Text | Editor::Select(_) | Editor::Secret { .. } => Value::String(input.to_string()),
        Editor::Bool => match input {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            other => return Err(format!("{other} is not a boolean")),
        },
        Editor::Number { min, max, .. } => {
            let parsed: f64 = input
                .parse()
                .map_err(|_| format!("{input} is not a number"))?;
            if min.is_some_and(|min| parsed < min) {
                return Err(format!("must be at least {}", min.unwrap()));
            }
            if max.is_some_and(|max| parsed > max) {
                return Err(format!("must be at most {}", max.unwrap()));
            }
            serde_json::Number::from_f64(parsed)
                .map(Value::Number)
                .ok_or_else(|| "not a finite number".to_string())?
        }
        Editor::Unsupported(kind) => {
            return Err(format!("this build cannot edit a {kind} field"))
        }
    };
    Ok(PathOp::Set {
        path: field.path.clone(),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(schema: Value, value: Value, user: Option<Value>) -> NamespaceView {
        NamespaceView {
            ns: "llm-deepseek".into(),
            schema,
            value,
            base: None,
            user,
            applies: "live".into(),
            secrets: Vec::new(),
            revision: 7,
        }
    }

    fn object(fields: Value) -> Value {
        serde_json::json!({ "type": "object", "dict": fields })
    }

    #[test]
    fn object_fields_flatten_with_their_metadata() {
        let schema = object(serde_json::json!({
            "baseUrl": { "type": "string", "meta": { "description": "API base URL" } },
            "timeout": { "type": "number", "meta": { "min": 1, "max": 600, "required": true } },
        }));
        let value = serde_json::json!({ "baseUrl": "https://api", "timeout": 30 });
        let fields = fields(&view(schema, value, None));

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].label, "baseUrl");
        assert_eq!(fields[0].description, "API base URL");
        assert_eq!(fields[0].editor, Editor::Text);
        assert_eq!(
            fields[1].editor,
            Editor::Number { min: Some(1.0), max: Some(600.0), step: None }
        );
        assert!(fields[1].required);
    }

    #[test]
    fn fields_keep_schema_declaration_order() {
        // Requires serde_json's `preserve_order`: without it object keys sort
        // alphabetically and the form silently reorders every settings page.
        let schema = serde_json::from_str::<Value>(
            r#"{"type":"object","dict":{
                "zulu":{"type":"string"},
                "alpha":{"type":"string"},
                "mike":{"type":"string"}}}"#,
        )
        .unwrap();
        let fields = fields(&view(schema, serde_json::json!({}), None));
        let labels: Vec<_> = fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["zulu", "alpha", "mike"]);
    }

    #[test]
    fn nested_objects_produce_dotted_paths() {
        let schema = object(serde_json::json!({
            "retry": object(serde_json::json!({ "attempts": { "type": "number" } })),
        }));
        let value = serde_json::json!({ "retry": { "attempts": 3 } });
        let fields = fields(&view(schema, value, None));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].label, "retry.attempts");
        assert_eq!(fields[0].path, vec!["retry", "attempts"]);
    }

    #[test]
    fn hidden_nodes_never_reach_the_form() {
        let schema = object(serde_json::json!({
            "visible": { "type": "string" },
            "internal": { "type": "string", "meta": { "hidden": true } },
        }));
        let fields = fields(&view(schema, serde_json::json!({}), None));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].label, "visible");
    }

    #[test]
    fn a_constant_union_becomes_a_picker() {
        let schema = object(serde_json::json!({
            "mode": { "type": "union", "list": [
                { "type": "const", "value": "native" },
                { "type": "const", "value": "ptc" },
            ] },
        }));
        let fields = fields(&view(schema, serde_json::json!({ "mode": "native" }), None));
        assert_eq!(
            fields[0].editor,
            Editor::Select(vec!["native".into(), "ptc".into()])
        );
    }

    #[test]
    fn a_union_of_shapes_is_not_a_picker() {
        let schema = object(serde_json::json!({
            "backend": { "type": "union", "list": [
                { "type": "string" },
                object(serde_json::json!({ "url": { "type": "string" } })),
            ] },
        }));
        let fields = fields(&view(schema, serde_json::json!({}), None));
        // Surfaced read-only rather than dropped: a missing row reads as "no such setting".
        assert_eq!(fields[0].editor, Editor::Unsupported("union".into()));
    }

    #[test]
    fn secrets_are_marked_and_never_display_a_value() {
        let mut v = view(
            object(serde_json::json!({ "apiKey": { "type": "string" } })),
            serde_json::json!({ "apiKey": "leaked-if-rendered" }),
            None,
        );
        v.secrets = vec![SecretView { path: vec!["apiKey".into()], set: true }];
        let fields = fields(&v);
        assert_eq!(fields[0].editor, Editor::Secret { set: true });
        assert_eq!(fields[0].display_value(), "••••••••");
    }

    #[test]
    fn the_user_layer_marks_overridden_fields() {
        let schema = object(serde_json::json!({
            "a": { "type": "string" },
            "b": { "type": "string" },
        }));
        let fields = fields(&view(
            schema,
            serde_json::json!({ "a": "from-user", "b": "from-default" }),
            Some(serde_json::json!({ "a": "from-user" })),
        ));
        assert!(fields[0].overridden);
        assert!(!fields[1].overridden);
    }

    #[test]
    fn intersect_members_merge_into_one_form() {
        let schema = serde_json::json!({ "type": "intersect", "list": [
            object(serde_json::json!({ "a": { "type": "string" } })),
            object(serde_json::json!({ "b": { "type": "boolean" } })),
        ] });
        let fields = fields(&view(schema, serde_json::json!({}), None));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1].editor, Editor::Bool);
    }

    #[test]
    fn a_localized_description_picks_a_readable_string() {
        let schema = object(serde_json::json!({
            "x": { "type": "string", "meta": { "description": { "zh": "中文", "en": "English" } } },
        }));
        let fields = fields(&view(schema, serde_json::json!({}), None));
        assert_eq!(fields[0].description, "English");
    }

    #[test]
    fn clearing_a_field_unsets_it_instead_of_pinning_an_empty_value() {
        let field = Field {
            path: vec!["baseUrl".into()],
            label: "baseUrl".into(),
            description: String::new(),
            editor: Editor::Text,
            required: false,
            disabled: false,
            value: Value::String("https://api".into()),
            overridden: true,
        };
        // Unset lets the composition base or schema default resurface.
        assert_eq!(
            edit_op(&field, "").unwrap(),
            PathOp::Unset { path: vec!["baseUrl".into()] }
        );
    }

    #[test]
    fn numeric_bounds_are_enforced_before_the_write_leaves() {
        let field = Field {
            path: vec!["timeout".into()],
            label: "timeout".into(),
            description: String::new(),
            editor: Editor::Number { min: Some(1.0), max: Some(600.0), step: None },
            required: false,
            disabled: false,
            value: Value::Null,
            overridden: false,
        };
        assert!(edit_op(&field, "0").unwrap_err().contains("at least 1"));
        assert!(edit_op(&field, "999").unwrap_err().contains("at most 600"));
        assert!(edit_op(&field, "abc").unwrap_err().contains("not a number"));
        assert_eq!(
            edit_op(&field, "30").unwrap(),
            PathOp::Set { path: vec!["timeout".into()], value: serde_json::json!(30.0) }
        );
    }

    #[test]
    fn ops_serialize_in_the_wire_shape() {
        let op = PathOp::Set {
            path: vec!["a".into(), "b".into()],
            value: serde_json::json!(true),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json["op"], "set");
        assert_eq!(json["path"], serde_json::json!(["a", "b"]));
        let unset = serde_json::to_value(PathOp::Unset { path: vec!["a".into()] }).unwrap();
        assert_eq!(unset["op"], "unset");
        assert!(unset.get("value").is_none());
    }
}
