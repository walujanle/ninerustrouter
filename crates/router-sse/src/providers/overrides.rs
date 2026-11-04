//! Per-provider user header overrides.
//!
//! The registry's `transport.headers` are the built-in baseline a provider
//! sends upstream. A user can override any of them from the dashboard, and the
//! override must win over everything the executor built — including auth, which
//! is why the blocked list refuses the request-structure and credential headers
//! rather than trying to merge around them.
//!
//! The store is `settings.providerOverrides`, keyed by canonical provider id:
//!
//! ```json
//! { "codex": { "headers": { "version": "0.160.0" } } }
//! ```
//!
//! Ordering is the insertion order of the settings blob, and an override with
//! no headers deletes the entry rather than storing an empty object — the
//! delete is what "restore the built-in defaults" means on the wire.

use serde_json::{Map, Value};

use crate::providers::registry::registry;

/// `MAX_HEADERS`.
pub const MAX_HEADERS: usize = 20;
/// `MAX_HEADER_VALUE_LENGTH`.
pub const MAX_HEADER_VALUE_LENGTH: usize = 8192;

/// `BLOCKED_HEADERS`: request-structure / auth headers an override must never
/// touch. Lower-cased; comparison is done on the lower-cased name.
pub const BLOCKED_HEADERS: [&str; 7] = [
    "host",
    "content-length",
    "content-type",
    "connection",
    "transfer-encoding",
    "authorization",
    "cookie",
];

/// `HEADER_NAME_RE`: `/^[A-Za-z0-9-]+$/`.
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `normalizeOverride({headers})`: validate, or `Err(message)`.
///
/// An override whose header list comes out empty is [`None`] — the caller
/// deletes the provider's entry.
pub fn normalize_override(value: &Value) -> Result<Option<Value>, String> {
    let Some(headers) = value.get("headers") else {
        return Ok(None);
    };
    if headers.is_null() {
        return Ok(None);
    }
    let Some(pairs) = headers.as_object() else {
        return Err("headers must be an object".to_string());
    };

    let entries: Vec<(&String, &Value)> = pairs
        .iter()
        .filter(|(_, v)| match v {
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
            _ => true,
        })
        .collect();
    if entries.len() > MAX_HEADERS {
        return Err(format!("Too many headers (max {MAX_HEADERS})"));
    }

    let mut clean = Map::new();
    for (name, value) in entries {
        if !is_valid_header_name(name) {
            return Err(format!("Invalid header name: {name}"));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("Invalid value for header {name}"));
        };
        if value.contains('\r') || value.contains('\n') {
            return Err(format!("Invalid value for header {name}"));
        }
        if value.chars().count() > MAX_HEADER_VALUE_LENGTH {
            return Err(format!(
                "Header {name} value too long (max {MAX_HEADER_VALUE_LENGTH})"
            ));
        }
        if BLOCKED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!("Header {name} cannot be overridden"));
        }
        clean.insert(name.clone(), Value::String(value.to_string()));
    }

    Ok(if clean.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "headers": Value::Object(clean) }))
    })
}

/// The user override map for one provider, or `None` when it has none.
///
/// `settings` is the whole settings object; an entry that is not an object with
/// a `headers` object is ignored, so a hand-edited blob cannot crash a request.
pub fn provider_overrides<'a>(
    settings: &'a Value,
    provider_id: &str,
) -> Option<&'a Map<String, Value>> {
    settings
        .get("providerOverrides")
        .and_then(|o| o.get(provider_id))
        .and_then(|o| o.get("headers"))
        .and_then(Value::as_object)
}

/// `builtinHeaders`: the registry transport's headers for a provider.
///
/// `{}` when the provider is unknown or declares none, so the UI can always
/// render the baseline it would restore.
pub fn builtin_headers(provider_id: &str) -> Map<String, Value> {
    registry()
        .transport(provider_id)
        .and_then(|t| t.headers.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_valid_override_is_kept_verbatim() {
        let out = normalize_override(&json!({
            "headers": { "X-Custom": "1", "version": "0.160.0" }
        }))
        .unwrap()
        .unwrap();
        assert_eq!(out["headers"]["X-Custom"], json!("1"));
        assert_eq!(out["headers"]["version"], json!("0.160.0"));
    }

    #[test]
    fn an_empty_header_set_normalizes_to_delete() {
        assert!(
            normalize_override(&json!({ "headers": {} }))
                .unwrap()
                .is_none()
        );
        assert!(
            normalize_override(&json!({ "headers": null }))
                .unwrap()
                .is_none()
        );
        assert!(normalize_override(&json!({})).unwrap().is_none());
    }

    #[test]
    fn blank_values_are_dropped_before_the_delete_check() {
        assert!(
            normalize_override(&json!({ "headers": { "X-A": "" } }))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_guard_rejects_names_values_and_blocked_headers() {
        assert!(
            normalize_override(&json!({ "headers": { "Bad Name": "1" } }))
                .unwrap_err()
                .contains("Invalid header name")
        );
        assert!(
            normalize_override(&json!({ "headers": { "X-A": "a\r\nb" } }))
                .unwrap_err()
                .contains("Invalid value")
        );
        assert!(
            normalize_override(&json!({ "headers": { "Authorization": "x" } }))
                .unwrap_err()
                .contains("cannot be overridden")
        );
        // Case-insensitive: `Content-Type` must be refused too.
        assert!(
            normalize_override(&json!({ "headers": { "Content-Type": "x" } }))
                .unwrap_err()
                .contains("cannot be overridden")
        );
    }

    #[test]
    fn too_many_headers_is_refused() {
        let mut headers = Map::new();
        for i in 0..=MAX_HEADERS {
            headers.insert(format!("X-{i}"), json!("v"));
        }
        let err = normalize_override(&json!({ "headers": headers })).unwrap_err();
        assert!(err.contains("Too many headers"));
    }

    #[test]
    fn the_override_lookup_is_keyed_by_provider() {
        let settings = json!({
            "providerOverrides": {
                "codex": { "headers": { "version": "1" } },
                "codebuddy-intl": { "headers": { "X-Product": "SaaS" } }
            }
        });
        let codex = provider_overrides(&settings, "codex").unwrap();
        assert_eq!(codex.get("version"), Some(&json!("1")));
        assert!(provider_overrides(&settings, "nope").is_none());
    }

    #[test]
    fn builtin_headers_come_from_the_registry_transport() {
        let codex = builtin_headers("codex");
        assert_eq!(codex.get("originator"), Some(&json!("codex_cli_rs")));
        assert!(builtin_headers("not-a-provider").is_empty());
    }
}
