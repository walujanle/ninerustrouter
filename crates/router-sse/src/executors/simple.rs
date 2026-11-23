//! The `DefaultExecutor` subclasses whose whole deviation is a URL or a body
//! rewrite.
//!
//! Each keeps the inherited send/retry loop and auth descriptor; only the hook
//! the provider actually needs is overridden.

use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::credentials::Credentials;
use crate::executors::default::DefaultExecutor;
use crate::executors::executor::{ExecError, Executor};
use crate::providers::model::Transport;
use crate::runtime_config::http_status;

// ─── codebuddy ───────────────────────────────────────────────────────────

/// The shared `stream: true` + `reasoning_summary` mirror the CodeBuddy
/// gateway needs. It rejects a non-stream chat request outright.
fn codebuddy_reasoning(transformed: &mut Value) {
    let effort = transformed
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string);
    match effort.as_deref() {
        Some("none") | Some("off") => {
            if let Some(obj) = transformed.as_object_mut() {
                obj.shift_remove("reasoning_effort");
            }
        }
        Some(_) => {
            if let Some(obj) = transformed.as_object_mut() {
                obj.insert("reasoning_summary".into(), json!("auto"));
            }
        }
        None => {}
    }
}

/// Sent when the client's own system prompt is an agent marker, or absent.
const NEUTRAL_AGENT_PROMPT: &str =
    "You are a helpful AI assistant that helps with software engineering tasks.";
/// The fallback leading system prompt when the client sent none.
const DEFAULT_CODEBUDDY_SYSTEM_PROMPT: &str = "You are CodeBuddy Code.";

/// The agent-identity markers Tencent's WAF rejects. `Simple case-insensitive
/// substring` rules, plus the two tag forms.
static AGENT_IDENTITY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"you are claude code",
        r"claude.?code.+official.+cli",
        r"anthropic.+official.+cli",
        r"anxthxropic.+official.+cli",
        r"you are (?:cursor|windsurf|cline|aider|continue|copilot|cody)",
        r"you are an? (?:ai )?(?:coding |code )?agent",
        r"cc_entrypoint\s*=\s*(?:cli|vscode|jetbrains|gui)",
        r"claude.?code.+issues",
        r"give feedback.+claude.?code",
        r"you are .{0,30}(?:powerful )?ai agent",
        r"orchestration capabilities",
        r"OhMyOpenCode",
        r"<agent-identity>",
        r"<Role>",
        r"<Behavior_Instructions>",
    ]
    .into_iter()
    .map(|p| Regex::new(&format!("(?i){p}")).expect("static pattern"))
    .collect()
});

fn is_agent_identity(text: &str) -> bool {
    AGENT_IDENTITY.iter().any(|re| re.is_match(text))
}

/// `m.content` as text: a string, or the `text` parts of an array joined with a
/// newline.
fn codebuddy_system_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `CodeBuddyIntlExecutor`: the international gateway, which wants user content
/// as typed blocks and answers 11101 without a leading system prompt.
///
/// The `X-Conversation-*` / `X-Request-ID` family comes from the registry's
/// `auth.hooks: ["codebuddyHeaders"]` via `apply_header_hooks`, so the header
/// set has one definition and the tests can read it back.
pub struct CodeBuddyIntlExecutor {
    inner: DefaultExecutor,
}

impl CodeBuddyIntlExecutor {
    pub fn new() -> Self {
        Self {
            inner: DefaultExecutor::new("codebuddy-intl"),
        }
    }
}

impl Default for CodeBuddyIntlExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Executor for CodeBuddyIntlExecutor {
    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn config(&self) -> &Transport {
        self.inner.config()
    }

    fn transform_request(
        &self,
        model: &str,
        body: Value,
        stream: bool,
        credentials: &Credentials,
    ) -> Value {
        let mut transformed = self
            .inner
            .transform_request(model, body, stream, credentials);
        if let Some(obj) = transformed.as_object_mut() {
            obj.insert("stream".into(), json!(true));
        }
        codebuddy_reasoning(&mut transformed);

        let source = transformed
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Tencent's WAF flags third-party CLI agent identity strings (Claude
        // Code, Cursor, ...) as unauthorized tooling or prompt injection and
        // rejects/bans the account. Neutralize only matching agent markers so a
        // user's own system prompt survives.
        let client_system: Vec<String> = source
            .iter()
            .filter(|m| {
                matches!(
                    m.get("role").and_then(Value::as_str),
                    Some("system" | "developer")
                )
            })
            .map(|m| {
                let text = codebuddy_system_text(m.get("content"));
                if is_agent_identity(&text) {
                    NEUTRAL_AGENT_PROMPT.to_string()
                } else {
                    text
                }
            })
            .filter(|t| !t.is_empty())
            .collect();
        let system_prompt = if client_system.is_empty() {
            DEFAULT_CODEBUDDY_SYSTEM_PROMPT.to_string()
        } else {
            client_system.join("\n\n")
        };

        let mut rebuilt = vec![json!({"role": "system", "content": system_prompt})];
        for message in source {
            let Some(obj) = message.as_object() else {
                continue;
            };
            let role = obj.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "system" || role == "developer" {
                continue;
            }
            let mut message = obj.clone();
            if role == "user"
                && let Some(Value::String(text)) = message.get("content").cloned()
            {
                message.insert("content".into(), json!([{ "type": "text", "text": text }]));
            }
            rebuilt.push(Value::Object(message));
        }
        if let Some(obj) = transformed.as_object_mut() {
            obj.insert("messages".into(), Value::Array(rebuilt));
        }
        transformed
    }

    /// Delegate to the default builder, which runs the registry's
    /// `auth.hooks` — `codebuddyHeaders` lives there.
    fn build_headers(
        &self,
        credentials: &Credentials,
        stream: bool,
        url: &str,
        model: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::header::HeaderMap, ExecError> {
        self.inner
            .build_headers(credentials, stream, url, model, body)
    }

    /// The `6004` frequency-limit envelope arrives as a `400`; report it as a
    /// `429` carrying the reset time the message names.
    fn parse_error(&self, status: u16, body_text: &str) -> Value {
        if !body_text.is_empty()
            && let Ok(parsed) = serde_json::from_str::<Value>(body_text)
        {
            let msg = parsed
                .get("msg")
                .or_else(|| parsed.get("message"))
                .or_else(|| parsed.get("error").and_then(|e| e.get("message")))
                .and_then(Value::as_str)
                .unwrap_or("");
            let code = parsed.get("code").and_then(Value::as_i64);
            let limit_hit = code == Some(6004)
                || ["超出频率限制", "frequency limit", "限额"]
                    .iter()
                    .any(|needle| msg.contains(needle));
            if limit_hit {
                let message = if msg.is_empty() {
                    "CodeBuddy frequency limit (6004)"
                } else {
                    msg
                };
                return match codebuddy_resets_at_ms(msg) {
                    Some(resets_at_ms) => json!({
                        "status": http_status::RATE_LIMITED,
                        "message": message,
                        "resetsAtMs": resets_at_ms,
                    }),
                    None => json!({ "status": http_status::RATE_LIMITED, "message": message }),
                };
            }
        }
        self.inner.parse_error(status, body_text)
    }
}

/// The reset instant a CodeBuddy limit message names, as epoch milliseconds.
///
/// The message carries a local `YYYY-MM-DD HH:MM:SS` with an optional
/// `UTC+8`-style suffix; without one the gateway's own zone (UTC+8) applies.
/// `None` when nothing parses, so the caller falls back to a plain cooldown
/// rather than inventing a time.
fn codebuddy_resets_at_ms(msg: &str) -> Option<i64> {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})(?:\s*UTC\+?([0-9:]+))?")
            .expect("static timestamp pattern")
    });
    static OFFSET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{1,2}):?(\d{2})?$").expect("static offset pattern"));

    let caps = STAMP.captures(msg)?;
    let date = caps.get(1)?.as_str();
    let time = caps.get(2)?.as_str();
    let naive =
        chrono::NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S")
            .ok()?;

    // No suffix at all: UTC+8. `8` is hours, `08:30` is hours and minutes.
    let seconds = match caps.get(3) {
        None => 8 * 3600,
        Some(raw) => {
            let raw = raw.as_str();
            let caps = OFFSET.captures(raw)?;
            let hours: i32 = caps.get(1)?.as_str().parse().ok()?;
            let minutes: i32 = caps.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            hours * 3600 + minutes * 60
        }
    };

    use chrono::TimeZone;
    let offset = chrono::FixedOffset::east_opt(seconds)?;
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&chrono::Utc).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebuddy_forces_stream_and_keeps_reasoning() {
        let executor = CodeBuddyIntlExecutor::new();
        let body = json!({"messages": [], "stream": false, "reasoning_effort": "high"});
        let out = executor.transform_request("m", body, false, &Credentials::default());
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["reasoning_summary"], json!("auto"));

        let body = json!({"messages": [], "reasoning_effort": "none"});
        let out = executor.transform_request("m", body, false, &Credentials::default());
        assert!(out.get("reasoning_effort").is_none());
        assert!(out.get("reasoning_summary").is_none());
    }

    #[test]
    fn codebuddy_intl_rebuilds_the_message_list() {
        let executor = CodeBuddyIntlExecutor::new();
        let body = json!({"messages": [
            {"role": "system", "content": "drop me"},
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [{"type": "text", "text": "hi"}]},
        ]});
        let out = executor.transform_request("m", body, false, &Credentials::default());
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        // A plain client system prompt is preserved verbatim, not replaced.
        assert_eq!(messages[0]["content"], json!("drop me"));
        assert_eq!(
            messages[1]["content"],
            json!([{"type": "text", "text": "hello"}])
        );
        assert_eq!(
            messages[2]["content"],
            json!([{"type": "text", "text": "hi"}])
        );
    }

    #[test]
    fn codebuddy_intl_neutralizes_agent_identity_but_keeps_user_prompts() {
        let executor = CodeBuddyIntlExecutor::new();
        let body = json!({"messages": [
            {"role": "system", "content": "You are Claude Code, Anthropic's official CLI."},
            {"role": "developer", "content": [{"type": "text", "text": "Answer in French."}]},
            {"role": "user", "content": "hi"},
        ]});
        let out = executor.transform_request("m", body, false, &Credentials::default());
        let system = out["messages"][0]["content"].as_str().unwrap();
        assert!(
            !system.to_ascii_lowercase().contains("claude code"),
            "{system}"
        );
        assert!(system.contains("You are a helpful AI assistant"));
        // The second, harmless system prompt survives alongside it.
        assert!(system.contains("Answer in French."));

        // No client system prompt at all falls back to the CodeBuddy identity.
        let out = executor.transform_request(
            "m",
            json!({"messages": [{"role": "user", "content": "hi"}]}),
            false,
            &Credentials::default(),
        );
        assert_eq!(
            out["messages"][0]["content"],
            json!("You are CodeBuddy Code.")
        );
    }

    #[test]
    fn codebuddy_intl_sends_the_plugin_channel_headers() {
        let executor = CodeBuddyIntlExecutor::new();
        let credentials = Credentials {
            access_token: Some("TESTTOKEN".into()),
            ..Default::default()
        };
        let headers = executor
            .build_headers(
                &credentials,
                true,
                "https://www.codebuddy.ai/v2/chat/completions",
                "deepseek-v4.1-flash",
                Some(&json!({})),
            )
            .unwrap();

        for key in [
            "x-request-id",
            "x-conversation-id",
            "x-conversation-request-id",
            "x-conversation-message-id",
        ] {
            let value = headers
                .get(key)
                .unwrap_or_else(|| panic!("{key} missing"))
                .to_str()
                .unwrap();
            assert!(!value.is_empty(), "{key} must be non-empty");
        }
        // One request id is reused across the request/conversation pair; the
        // conversation id is its own value.
        assert_eq!(
            headers.get("x-request-id"),
            headers.get("x-conversation-request-id")
        );
        assert_eq!(
            headers.get("x-request-id"),
            headers.get("x-conversation-message-id")
        );
        assert_ne!(
            headers.get("x-conversation-id"),
            headers.get("x-conversation-message-id")
        );
        // The hook runs before auth, so the bearer token survives.
        assert_eq!(headers.get("authorization").unwrap(), "Bearer TESTTOKEN");

        // Fresh ids per request.
        let again = executor
            .build_headers(&credentials, true, "https://x", "m", Some(&json!({})))
            .unwrap();
        assert_ne!(headers.get("x-request-id"), again.get("x-request-id"));
        assert_ne!(
            headers.get("x-conversation-id"),
            again.get("x-conversation-id")
        );
    }

    #[test]
    fn codebuddy_6004_is_reported_as_a_rate_limit_with_the_reset_time() {
        let executor = CodeBuddyIntlExecutor::new();
        let out = executor.parse_error(
            400,
            r#"{"code":6004,"msg":"超出频率限制 2026-10-28 18:30:00 UTC+8"}"#,
        );
        assert_eq!(out["status"], json!(429));
        assert!(out["resetsAtMs"].is_i64(), "{out}");
        // 2026-10-28 18:30:00 at UTC+8 is 10:30:00Z.
        assert_eq!(out["resetsAtMs"], json!(1793183400000i64));
    }

    #[test]
    fn a_6004_wording_without_a_suffix_assumes_utc_plus_8() {
        let executor = CodeBuddyIntlExecutor::new();
        let out = executor.parse_error(
            400,
            r#"{"code":6004,"msg":"frequency limit 2026-10-28 18:30:00"}"#,
        );
        assert_eq!(out["status"], json!(429));
        assert_eq!(out["resetsAtMs"], json!(1793183400000i64));
    }

    #[test]
    fn a_non_limit_body_falls_through_to_the_default_parse() {
        let executor = CodeBuddyIntlExecutor::new();
        let out = executor.parse_error(400, r#"{"code":11101,"msg":"invalid request"}"#);
        assert_eq!(out["status"], json!(400));
        assert_eq!(
            out["message"],
            json!(r#"{"code":11101,"msg":"invalid request"}"#)
        );
    }

    #[test]
    fn the_simple_executor_reads_its_transport_from_the_registry() {
        assert_eq!(
            CodeBuddyIntlExecutor::new().config().format_or_default(),
            "openai"
        );
        assert_eq!(CodeBuddyIntlExecutor::new().provider(), "codebuddy-intl");
    }
}
