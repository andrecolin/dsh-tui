//! The Models settings page: joining the provider directory to settings and credentials.
//!
//! Mirrors the web client's store: `llm.listProviders` (routes the adapter registry knows)
//! joined with `llm.listConfigurableProviders` (routes configuration can activate), each
//! entry resolved against its settings namespace profile and the credential its profile
//! names.

use serde::Deserialize;
use serde_json::Value;

use crate::settings::NamespaceView;

/// A route the adapter registry has registered.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// A route configuration can activate.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigurableProvider {
    pub provider: String,
    #[serde(default, rename = "displayName")]
    pub display_name: String,
    #[serde(default, rename = "settingsNs")]
    pub settings_ns: String,
    /// Path from the namespace section root to this provider's profile; empty when the
    /// whole section is the profile.
    #[serde(default, rename = "settingsPath")]
    pub settings_path: Vec<String>,
    /// Whether the adapter knows this route only because configuration declared it.
    #[serde(default)]
    pub declared: Option<bool>,
}

/// Whether resolving a credential reference would currently return a value.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CredentialInfo {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub writable: bool,
}

/// One joined row of the Models page.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRow {
    pub provider: String,
    pub display_name: String,
    /// The adapter registry knows this route, so it can serve requests.
    pub registered: bool,
    /// Configuration stores a profile for this route.
    pub configured: bool,
    /// The profile lives in the user layer and nothing beneath it, so it can be removed.
    pub removable: bool,
    /// The credential reference this route resolves through.
    pub key_ref: String,
    /// Whether the reference was named by the profile rather than derived from the route.
    pub key_ref_named: bool,
    pub credential: Option<CredentialInfo>,
}

impl ProviderRow {
    /// Whether this row can serve model requests as it stands: the route is registered and
    /// its credential resolves.
    pub fn ready(&self) -> bool {
        self.registered && self.credential.as_ref().is_some_and(|c| c.configured)
    }

    /// One-line status for the row.
    pub fn status(&self) -> &'static str {
        if !self.configured {
            "not configured"
        } else if !self.registered {
            "configured, adapter not registered"
        } else if self.credential.as_ref().is_some_and(|c| c.configured) {
            "ready"
        } else {
            "needs an API key"
        }
    }
}

/// The credential reference derived from a route id.
///
/// `minimax-cn` → `MINIMAX_CN_API_KEY`. Used when the profile does not name one itself.
pub fn derive_key_ref(provider: &str) -> String {
    let upper = provider.to_uppercase();
    let mut out = String::with_capacity(upper.len() + 8);
    let mut pending_separator = false;
    for ch in upper.chars() {
        if ch.is_ascii_uppercase() || ch.is_ascii_digit() {
            if pending_separator && !out.is_empty() {
                out.push('_');
            }
            pending_separator = false;
            out.push(ch);
        } else {
            pending_separator = true;
        }
    }
    // A trailing run of separators collapses the same way a leading one does.
    out.push_str("_API_KEY");
    out
}

/// Read a value at a path, matching the web client's `getPath`.
pub fn get_path<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = root;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

/// Whether a path exists in a layer.
pub fn has_path(root: Option<&Value>, path: &[String]) -> bool {
    root.and_then(|value| get_path(value, path)).is_some()
}

/// The credential reference a resolved profile names through its `apiKeyEnv` field.
fn api_key_env(namespace: Option<&NamespaceView>, path: &[String]) -> Option<String> {
    let profile = get_path(&namespace?.value, path)?;
    let reference = profile.get("apiKeyEnv")?.as_str()?;
    (!reference.is_empty()).then(|| reference.to_string())
}

/// Join the provider directory into rows.
///
/// `credentials` may be empty: credential state is an enrichment for this page, and neither
/// a business rejection nor a transport failure should fail the whole load.
pub fn rows(
    registered: &[ProviderInfo],
    configurable: &[ConfigurableProvider],
    namespaces: &[NamespaceView],
    credentials: &std::collections::HashMap<String, CredentialInfo>,
) -> Vec<ProviderRow> {
    configurable
        .iter()
        .map(|entry| {
            let namespace = namespaces.iter().find(|ns| ns.ns == entry.settings_ns);
            let configured = namespace.is_some_and(|ns| {
                entry.settings_path.is_empty()
                    || get_path(&ns.value, &entry.settings_path).is_some()
            });
            let removable = namespace.is_some_and(|ns| {
                !entry.settings_path.is_empty()
                    && has_path(ns.user.as_ref(), &entry.settings_path)
                    && !has_path(ns.base.as_ref(), &entry.settings_path)
            });
            let named = api_key_env(namespace, &entry.settings_path);
            let key_ref_named = named.is_some();
            let key_ref = named.unwrap_or_else(|| derive_key_ref(&entry.provider));

            ProviderRow {
                display_name: if entry.display_name.is_empty() {
                    entry.provider.clone()
                } else {
                    entry.display_name.clone()
                },
                registered: registered.iter().any(|p| p.id == entry.provider),
                configured,
                removable,
                credential: credentials.get(&key_ref).cloned(),
                key_ref,
                key_ref_named,
                provider: entry.provider.clone(),
            }
        })
        .collect()
}

/// Every credential reference the rows resolve through, deduplicated.
pub fn credential_refs(
    configurable: &[ConfigurableProvider],
    namespaces: &[NamespaceView],
) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for entry in configurable {
        let namespace = namespaces.iter().find(|ns| ns.ns == entry.settings_ns);
        let reference = api_key_env(namespace, &entry.settings_path)
            .unwrap_or_else(|| derive_key_ref(&entry.provider));
        if !refs.contains(&reference) {
            refs.push(reference);
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn namespace(ns: &str, value: Value, user: Option<Value>, base: Option<Value>) -> NamespaceView {
        NamespaceView {
            ns: ns.into(),
            schema: Value::Null,
            value,
            base,
            user,
            applies: "live".into(),
            secrets: Vec::new(),
            revision: 1,
        }
    }

    fn entry(provider: &str, ns: &str, path: &[&str]) -> ConfigurableProvider {
        ConfigurableProvider {
            provider: provider.into(),
            display_name: String::new(),
            settings_ns: ns.into(),
            settings_path: path.iter().map(|s| s.to_string()).collect(),
            declared: None,
        }
    }

    #[test]
    fn key_references_derive_from_the_route_id() {
        assert_eq!(derive_key_ref("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(derive_key_ref("minimax-cn"), "MINIMAX_CN_API_KEY");
        assert_eq!(derive_key_ref("open.router"), "OPEN_ROUTER_API_KEY");
        assert_eq!(derive_key_ref("gpt4"), "GPT4_API_KEY");
    }

    #[test]
    fn a_profile_named_reference_beats_the_derived_one() {
        let namespaces = vec![namespace(
            "llm-custom",
            serde_json::json!({ "providers": { "gw": { "apiKeyEnv": "MY_GATEWAY_TOKEN" } } }),
            None,
            None,
        )];
        let configurable = vec![entry("gw", "llm-custom", &["providers", "gw"])];
        let rows = rows(&[], &configurable, &namespaces, &HashMap::new());
        assert_eq!(rows[0].key_ref, "MY_GATEWAY_TOKEN");
        assert!(rows[0].key_ref_named);
    }

    #[test]
    fn a_route_without_a_stored_profile_is_not_configured() {
        let namespaces = vec![namespace("llm-x", serde_json::json!({ "providers": {} }), None, None)];
        let configurable = vec![entry("absent", "llm-x", &["providers", "absent"])];
        let rows = rows(&[], &configurable, &namespaces, &HashMap::new());
        assert!(!rows[0].configured);
        assert_eq!(rows[0].status(), "not configured");
    }

    #[test]
    fn a_whole_section_profile_counts_as_configured() {
        // An empty settingsPath means the section itself is the profile.
        let namespaces = vec![namespace("llm-deepseek", serde_json::json!({}), None, None)];
        let configurable = vec![entry("deepseek", "llm-deepseek", &[])];
        let rows = rows(&[], &configurable, &namespaces, &HashMap::new());
        assert!(rows[0].configured);
    }

    #[test]
    fn only_a_user_layer_profile_with_nothing_beneath_it_is_removable() {
        let path = ["providers", "gw"];
        let user_only = vec![namespace(
            "llm-x",
            serde_json::json!({ "providers": { "gw": {} } }),
            Some(serde_json::json!({ "providers": { "gw": {} } })),
            None,
        )];
        let configurable = vec![entry("gw", "llm-x", &path)];
        assert!(rows(&[], &configurable, &user_only, &HashMap::new())[0].removable);

        // Shipped underneath: removing the user layer would resurface the base, so the
        // row is not removable.
        let shadowing_base = vec![namespace(
            "llm-x",
            serde_json::json!({ "providers": { "gw": {} } }),
            Some(serde_json::json!({ "providers": { "gw": {} } })),
            Some(serde_json::json!({ "providers": { "gw": {} } })),
        )];
        assert!(!rows(&[], &configurable, &shadowing_base, &HashMap::new())[0].removable);
    }

    #[test]
    fn readiness_needs_both_registration_and_a_credential() {
        let namespaces = vec![namespace("llm-deepseek", serde_json::json!({}), None, None)];
        let configurable = vec![entry("deepseek", "llm-deepseek", &[])];
        let registered = vec![ProviderInfo { id: "deepseek".into(), name: "DeepSeek".into() }];

        let mut credentials = HashMap::new();
        credentials.insert(
            "DEEPSEEK_API_KEY".to_string(),
            CredentialInfo { configured: true, source: Some("env".into()), writable: true },
        );

        let ready = rows(&registered, &configurable, &namespaces, &credentials);
        assert!(ready[0].ready());
        assert_eq!(ready[0].status(), "ready");

        // Registered but no credential.
        let unkeyed = rows(&registered, &configurable, &namespaces, &HashMap::new());
        assert!(!unkeyed[0].ready());
        assert_eq!(unkeyed[0].status(), "needs an API key");

        // Credentialed but the adapter never registered the route.
        let unregistered = rows(&[], &configurable, &namespaces, &credentials);
        assert!(!unregistered[0].ready());
        assert_eq!(unregistered[0].status(), "configured, adapter not registered");
    }

    #[test]
    fn missing_credential_state_still_produces_rows() {
        // Credential lookup is an enrichment: a failure must not empty the page.
        let namespaces = vec![namespace("llm-deepseek", serde_json::json!({}), None, None)];
        let configurable = vec![entry("deepseek", "llm-deepseek", &[])];
        let rows = rows(&[], &configurable, &namespaces, &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].credential.is_none());
    }

    #[test]
    fn credential_references_are_deduplicated() {
        let namespaces = vec![namespace(
            "llm-x",
            serde_json::json!({ "providers": {
                "a": { "apiKeyEnv": "SHARED" },
                "b": { "apiKeyEnv": "SHARED" },
                "c": {}
            } }),
            None,
            None,
        )];
        let configurable = vec![
            entry("a", "llm-x", &["providers", "a"]),
            entry("b", "llm-x", &["providers", "b"]),
            entry("c", "llm-x", &["providers", "c"]),
        ];
        let refs = credential_refs(&configurable, &namespaces);
        assert_eq!(refs, vec!["SHARED", "C_API_KEY"]);
    }

    #[test]
    fn a_displayless_entry_falls_back_to_its_route_id() {
        let configurable = vec![entry("some-route", "llm-x", &[])];
        let rows = rows(&[], &configurable, &[], &HashMap::new());
        assert_eq!(rows[0].display_name, "some-route");
    }
}
