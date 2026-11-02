//! Flatten an OpenAI `messages[]` into the single `prompt` string the
//! chat.deepseek.com web endpoint accepts, and read the model id into the
//! endpoint's three booleans.

use serde_json::Value;

/// Upstream's `DEFAULT_AUTO_HISTORY_WINDOW`: how many trailing turns a
/// multi-turn conversation replays when the connection sets no explicit window.
pub const DEFAULT_AUTO_HISTORY_WINDOW: usize = 20;

/// `extractMessageText(content)`: the joined `text` blocks, else the string form.
pub fn extract_message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .map(|item| item.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        Some(other) => crate::translator::concerns::primitives::js_string(other),
        None => String::new(),
    }
}

/// `resolveModelOptions(model, body)`: the model id and body flags into the
/// endpoint's `model_type` / `thinking_enabled` / `search_enabled`.
pub fn resolve_model_options(model: &str, body: &Value) -> (String, bool, bool) {
    let lower = model.to_lowercase();
    let model_type = if lower.contains("pro") || lower.contains("expert") {
        "expert"
    } else {
        "default"
    };
    let thinking = lower.contains("r1")
        || lower.contains("think")
        || lower.contains("reason")
        || body.get("thinking_enabled").and_then(Value::as_bool) == Some(true)
        || body.get("thinking").and_then(Value::as_bool) == Some(true)
        || body.get("reasoning_effort").is_some_and(|v| !v.is_null());
    let search = lower.contains("search")
        || body.get("search_enabled").and_then(Value::as_bool) == Some(true)
        || body.get("search").and_then(Value::as_bool) == Some(true)
        || body.get("web_search").and_then(Value::as_bool) == Some(true);
    (model_type.to_string(), thinking, search)
}

/// `messagesToPrompt(messages, historyWindow)`.
///
/// A single-turn request keeps the minimal shape — system prompt, then the last
/// user message. A multi-turn one stitches a bounded role-tagged transcript, so
/// an agentic client that never sends `tools[]` does not lose the original task
/// after a couple of turns.
pub fn messages_to_prompt(messages: &[Value], history_window: usize) -> String {
    if messages.is_empty() {
        return String::new();
    }

    let mut system_parts: Vec<String> = Vec::new();
    // (role, text) — `tool` turns are folded in as plain text.
    let mut conversation: Vec<(&'static str, String)> = Vec::new();
    let mut last_user_content = String::new();

    for message in messages {
        let text = extract_message_text(message.get("content"))
            .trim()
            .to_string();
        match message.get("role").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            // `user` is the only role that also updates the single-turn
            // fallback, so it cannot collapse into the shared arm.
            "user" if !text.is_empty() => {
                conversation.push(("user", text.clone()));
                last_user_content = text;
            }
            "assistant" if !text.is_empty() => conversation.push(("assistant", text)),
            // Tool results have no native slot in the single-prompt format, so
            // they fold into the transcript as plain text.
            "tool" if !text.is_empty() => {
                let name = message
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                conversation.push(("tool", format!("({name}) {text}")));
            }
            _ => {}
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if !system_parts.is_empty() {
        parts.push(system_parts.join("\n\n"));
    }

    let effective_window = if history_window > 0 {
        history_window
    } else if conversation.len() > 1 {
        DEFAULT_AUTO_HISTORY_WINDOW
    } else {
        0
    };

    if effective_window > 0 && conversation.len() > 1 {
        let start = conversation.len().saturating_sub(effective_window);
        let transcript = conversation[start..]
            .iter()
            .map(|(role, text)| match *role {
                "assistant" => format!("Assistant: {text}"),
                "tool" => format!("Tool result {text}"),
                _ => format!("User: {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        parts.push(transcript);
    } else if !last_user_content.is_empty() {
        parts.push(last_user_content);
    }

    strip_markdown_images(&parts.join("\n\n"))
}

/// `replace(/!\[.*?\]\(.*?\)/g, "")` — non-greedy on both sides.
fn strip_markdown_images(input: &str) -> String {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"!\[.*?\]\(.*?\)").expect("static markdown-image pattern")
    });
    PATTERN.replace_all(input, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_single_turn_keeps_only_the_system_prompt_and_last_user_message() {
        // One user turn only: the transcript path needs a second entry to fire.
        let messages = vec![
            json!({"role": "system", "content": "be terse"}),
            json!({"role": "user", "content": "only"}),
        ];
        assert_eq!(messages_to_prompt(&messages, 0), "be terse\n\nonly");
    }

    #[test]
    fn two_user_turns_switch_to_the_auto_window_transcript() {
        let messages = vec![
            json!({"role": "user", "content": "first"}),
            json!({"role": "user", "content": "second"}),
        ];
        assert_eq!(
            messages_to_prompt(&messages, 0),
            "User: first\n\nUser: second"
        );
    }

    #[test]
    fn a_multi_turn_conversation_replays_a_bounded_transcript() {
        let messages = vec![
            json!({"role": "user", "content": "first"}),
            json!({"role": "assistant", "content": "answer"}),
            json!({"role": "user", "content": "second"}),
        ];
        assert_eq!(
            messages_to_prompt(&messages, 0),
            "User: first\n\nAssistant: answer\n\nUser: second"
        );
    }

    #[test]
    fn an_explicit_window_trims_the_transcript() {
        let messages = vec![
            json!({"role": "user", "content": "a"}),
            json!({"role": "assistant", "content": "b"}),
            json!({"role": "user", "content": "c"}),
        ];
        assert_eq!(messages_to_prompt(&messages, 1), "User: c");
    }

    #[test]
    fn typed_text_blocks_join_with_a_newline() {
        let messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "one"},
                {"type": "image_url", "image_url": {"url": "x"}},
                {"type": "text", "text": "two"},
            ],
        })];
        assert_eq!(messages_to_prompt(&messages, 0), "one\ntwo");
    }

    #[test]
    fn markdown_images_are_stripped() {
        let messages = vec![json!({"role": "user", "content": "see ![a](b) now"})];
        assert_eq!(messages_to_prompt(&messages, 0), "see  now");
    }

    #[test]
    fn tool_results_fold_into_the_transcript() {
        let messages = vec![
            json!({"role": "user", "content": "run it"}),
            json!({"role": "assistant", "content": "ok"}),
            json!({"role": "tool", "name": "shell", "content": "output"}),
        ];
        assert_eq!(
            messages_to_prompt(&messages, 0),
            "User: run it\n\nAssistant: ok\n\nTool result (shell) output"
        );
    }

    #[test]
    fn model_options_read_the_id_and_the_body() {
        assert_eq!(
            resolve_model_options("deepseek-v4-pro-think-search", &json!({})),
            ("expert".to_string(), true, true)
        );
        assert_eq!(
            resolve_model_options("deepseek-chat", &json!({})),
            ("default".to_string(), false, false)
        );
        assert_eq!(
            resolve_model_options(
                "deepseek-chat",
                &json!({"thinking_enabled": true, "search_enabled": true})
            ),
            ("default".to_string(), true, true)
        );
        assert_eq!(
            resolve_model_options("DeepSeek-R1", &json!({})),
            ("default".to_string(), true, false)
        );
    }
}
