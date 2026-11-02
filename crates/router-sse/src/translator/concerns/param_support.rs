//! Strip request params a provider/model rejects upstream.
//!
//! Config-driven on purpose: a new quirk is a new rule, not another
//! ad-hoc deletion scattered through the executors. The rules are data, so the
//! only logic is the match test and the per-rule action.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// The model matcher for a rule.
enum Matcher {
    /// No `match` field: the rule applies to every model of the provider.
    Any,
    /// A regex tested against the model id.
    Re(&'static str),
}

impl Matcher {
    fn matches(&self, model: &str) -> bool {
        match self {
            Matcher::Any => true,
            Matcher::Re(pattern) => compile(pattern).is_match(model),
        }
    }
}

/// One strip rule.
struct Rule {
    provider: Option<&'static str>,
    matcher: Matcher,
    drop: &'static [&'static str],
    drop_message_fields: &'static [&'static str],
    /// `renameMessageFields`: assistant-turn field renames, applied before the
    /// drops so a rename target the rule also drops still lands.
    rename_message_fields: &'static [(&'static str, &'static str)],
}

/// Every strip rule. The order does not matter (rules are independent).
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // All Claude models: temperature deprecated/rejected upstream (Anthropic 400).
        Rule {
            provider: None,
            matcher: Matcher::Re("(?i)claude"),
            drop: &["temperature"],
            drop_message_fields: &[],
            rename_message_fields: &[],
        },
        // Strict validators reject replayed reasoning on assistant turns.
        Rule {
            provider: Some("mistral"),
            matcher: Matcher::Any,
            drop: &[],
            drop_message_fields: &["reasoning_content", "reasoning", "reasoning_details"],
            rename_message_fields: &[],
        },
        // NVIDIA NIM treats `reasoning` and `reasoning_content` as aliases for one
        // field; sending both makes its deserializer report a duplicate field.
        // Keep the OpenAI canonical spelling on assistant history.
        Rule {
            provider: Some("nvidia"),
            matcher: Matcher::Any,
            drop: &[],
            drop_message_fields: &[],
            rename_message_fields: &[("reasoning", "reasoning_content")],
        },
        // OpenCode Go's relay rejects `response_format` for the deepseek family
        // with a 400 that names neither the field nor the reason. Every other
        // family on the same relay accepts it.
        Rule {
            provider: Some("opencode-go"),
            matcher: Matcher::Re("(?i)^deepseek"),
            drop: &["response_format"],
            drop_message_fields: &[],
            rename_message_fields: &[],
        },
    ]
});

/// Compiled rule patterns. The set is tiny and all patterns are literals, so a
/// process-wide map built on first use is enough; `Regex` clones share the
/// compiled program.
fn compile(pattern: &'static str) -> Regex {
    static CACHE: LazyLock<dashmap::DashMap<&'static str, Regex>> =
        LazyLock::new(dashmap::DashMap::new);
    CACHE
        .entry(pattern)
        .or_insert_with(|| Regex::new(pattern).expect("static strip-rule pattern"))
        .clone()
}

/// `typeof v === "string" && !v.trim()`.
fn is_blank_string(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.trim().is_empty())
}

/// Strip the params this provider/model rejects, in place.
pub fn strip_unsupported_params(provider: Option<&str>, model: &str, body: &mut Value) {
    if model.is_empty() || !body.is_object() {
        return;
    }
    for rule in RULES.iter() {
        if rule.provider.is_some_and(|p| Some(p) != provider) {
            continue;
        }
        if !rule.matcher.matches(model) {
            continue;
        }
        if let Some(obj) = body.as_object_mut() {
            for key in rule.drop {
                obj.remove(*key);
            }
        }
        if (rule.rename_message_fields.is_empty() && rule.drop_message_fields.is_empty())
            || !body.get("messages").is_some_and(Value::is_array)
        {
            continue;
        }
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for msg in messages {
                if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let Some(obj) = msg.as_object_mut() else {
                    continue;
                };
                for (from, to) in rule.rename_message_fields {
                    if !obj.contains_key(*from) {
                        continue;
                    }
                    // A rename never overwrites a value the client already sent;
                    // an empty string counts as absent.
                    let existing_filled = obj
                        .get(*to)
                        .is_some_and(|v| !v.is_null() && !is_blank_string(v));
                    if !existing_filled {
                        let value = obj.get(*from).cloned().unwrap_or(Value::Null);
                        obj.insert((*to).to_string(), value);
                    }
                    obj.remove(*from);
                }
                for key in rule.drop_message_fields {
                    obj.remove(*key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_models_lose_temperature_everywhere() {
        let mut body = json!({"temperature": 0.7});
        strip_unsupported_params(None, "claude-sonnet-4.5", &mut body);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn a_non_claude_model_keeps_temperature() {
        let mut body = json!({"temperature": 0.7});
        strip_unsupported_params(Some("openai"), "gpt-5", &mut body);
        assert_eq!(body["temperature"], json!(0.7));
    }

    #[test]
    fn opencode_go_deepseek_loses_response_format_only() {
        let mut body = json!({"response_format": {"type": "json_object"}});
        strip_unsupported_params(Some("opencode-go"), "deepseek-v4.1-flash", &mut body);
        assert!(body.get("response_format").is_none());

        // A non-deepseek model on the same relay keeps it.
        let mut body = json!({"response_format": {"type": "json_object"}});
        strip_unsupported_params(Some("opencode-go"), "glm-5.3-flash", &mut body);
        assert!(body.get("response_format").is_some());

        // Another provider's deepseek model is untouched.
        let mut body = json!({"response_format": {"type": "json_object"}});
        strip_unsupported_params(Some("deepseek"), "deepseek-chat", &mut body);
        assert!(body.get("response_format").is_some());
    }

    #[test]
    fn rules_are_scoped_to_their_provider() {
        let mut body = json!({"thinking": {"type": "enabled"}});
        strip_unsupported_params(Some("anthropic"), "claude-sonnet-4.5", &mut body);
        assert!(body.get("thinking").is_some());
    }

    #[test]
    fn nvidia_renames_reasoning_to_reasoning_content() {
        // Only `reasoning` present: it is renamed, not duplicated.
        let mut body = json!({"messages": [
            {"role": "assistant", "reasoning": "step 1", "content": "ok"},
        ]});
        strip_unsupported_params(Some("nvidia"), "moonshotai/kimi-k2.6", &mut body);
        assert!(body["messages"][0].get("reasoning").is_none());
        assert_eq!(body["messages"][0]["reasoning_content"], json!("step 1"));

        // Both present: the client's own reasoning_content wins, the alias goes.
        let mut body = json!({"messages": [
            {"role": "assistant", "reasoning": "alias", "reasoning_content": "canonical"},
        ]});
        strip_unsupported_params(Some("nvidia"), "moonshotai/kimi-k2.6", &mut body);
        assert!(body["messages"][0].get("reasoning").is_none());
        assert_eq!(body["messages"][0]["reasoning_content"], json!("canonical"));

        // A blank reasoning_content counts as absent, so the alias fills it.
        let mut body = json!({"messages": [
            {"role": "assistant", "reasoning": "alias", "reasoning_content": "   "},
        ]});
        strip_unsupported_params(Some("nvidia"), "moonshotai/kimi-k2.6", &mut body);
        assert_eq!(body["messages"][0]["reasoning_content"], json!("alias"));
    }

    #[test]
    fn nvidia_rename_leaves_user_turns_and_other_providers_alone() {
        let mut body = json!({"messages": [{"role": "user", "reasoning": "keep"}]});
        strip_unsupported_params(Some("nvidia"), "m", &mut body);
        assert_eq!(body["messages"][0]["reasoning"], json!("keep"));

        let mut body = json!({"messages": [{"role": "assistant", "reasoning": "keep"}]});
        strip_unsupported_params(Some("deepseek"), "deepseek-chat", &mut body);
        assert_eq!(body["messages"][0]["reasoning"], json!("keep"));
    }
}
