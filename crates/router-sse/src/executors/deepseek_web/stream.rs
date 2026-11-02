//! Convert DeepSeek's web SSE frames into OpenAI `chat.completion.chunk`
//! frames, and collect the same content for a non-streaming caller.
//!
//! The upstream frame is `{"p": "<path>", "o": "<op>", "v": <payload>}`, not an
//! OpenAI delta:
//!
//! - `v.response.fragments[]` and `p: "response/fragments"` carry `{type, content}`,
//!   where `type` is `THINK` or `ANSWER`/`RESPONSE`.
//! - `p: "response/status"` with `v: "FINISHED"` is the terminal event. Search
//!   results may still follow it, so a short drain window is kept open before
//!   `[DONE]` — a stream that never closes hangs every OpenAI SDK client.
//! - `p: "response/search_results"` is `[{title, url, cite_index}]`, either the
//!   whole list or per-index patches when `o == "BATCH"`.
//!
//! `ponytail:` citations are emitted as a trailing markdown list, not as a
//! separate `annotations` field — no client in this tree reads the latter.

use serde_json::{Map, Value, json};

use crate::executors::executor::ByteStream;
use crate::utils::sse::{SSE_DONE, sse_chunk};

/// How long to keep reading after `FINISHED` for late `search_results`.
pub const FINISHED_DRAIN_MS: u64 = 750;

/// `isThinkingModel(model)`.
pub fn is_thinking_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("think") || m.contains("r1") || m.contains("reason")
}

/// `isSearchModel(model)`.
pub fn is_search_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("search") || m.contains("fold")
}

/// `cleanDeepSeekToken(text)`.
pub fn clean_token(text: &str) -> String {
    static PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(SEARCH|WEB_SEARCH|SEARCHING)\s*").expect("static token prefix")
    });
    PREFIX
        .replace(&text.replace("FINISHED", ""), "")
        .into_owned()
}

/// `formatStreamContent(raw, model)`: strip the transport tokens, then rewrite
/// `[citation:N]` to `[N]` for a search model.
pub fn format_stream_content(raw: &str, model: &str) -> String {
    let text = clean_token(raw);
    if !is_search_model(model) {
        return text;
    }
    if model.to_lowercase().contains("search-silent") {
        return markdown_citation(&text, "");
    }
    markdown_citation(&text, "[$1]")
}

fn markdown_citation(text: &str, replacement: &str) -> String {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[citation:(\d+)\]").expect("static citation pattern")
    });
    PATTERN.replace_all(text, replacement).into_owned()
}

/// One cited source.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub cite_index: Option<i64>,
    pub title: String,
    pub url: String,
}

impl SearchResult {
    fn from_value(value: &Value) -> Self {
        Self {
            cite_index: value.get("cite_index").and_then(Value::as_i64),
            title: value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            url: value
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// `appendSearchCitations(results, model)`: the trailing `[n]: [title](url)` list.
pub fn append_search_citations(results: &[SearchResult], model: &str) -> String {
    if results.is_empty() || model.to_lowercase().contains("search-silent") {
        return String::new();
    }
    let mut cited: Vec<&SearchResult> = results.iter().filter(|r| r.cite_index.is_some()).collect();
    cited.sort_by_key(|r| r.cite_index.unwrap_or(0));
    cited
        .iter()
        .map(|r| format!("[{}]: [{}]({})", r.cite_index.unwrap_or(0), r.title, r.url))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the frame walk decided.
#[derive(Debug, Default)]
pub struct Collected {
    pub content: String,
    pub reasoning: String,
    pub saw_finished: bool,
}

/// The streaming path's per-frame state, kept separate so the non-streaming
/// collector cannot drift.
#[derive(Default)]
struct FrameState {
    path_thinking: bool,
    search_results: Vec<SearchResult>,
}

impl FrameState {
    /// `applyFragmentType`: an explicit fragment type re-pins the current path.
    fn apply_fragment_type(&mut self, fragment: &Value) {
        match fragment
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase()
            .as_str()
        {
            "THINK" => self.path_thinking = true,
            "ANSWER" | "RESPONSE" => self.path_thinking = false,
            _ => {}
        }
    }
}

/// Walk one decoded `data:` payload, folding it into `state`/`collected`.
///
/// Returns the deltas the streaming emitter still has to send; the collector
/// path ignores them.
fn apply_payload(
    data: &Value,
    state: &mut FrameState,
    collected: &mut Collected,
    model: &str,
) -> Vec<(bool, String)> {
    let mut deltas = Vec::new();
    let path = data.get("p").and_then(Value::as_str).unwrap_or("");
    let op = data.get("o").and_then(Value::as_str);
    let value = data.get("v");

    if let Some(response) = value
        .filter(|v| v.is_object())
        .and_then(|v| v.get("response"))
    {
        if response.get("thinking_enabled").and_then(Value::as_bool) == Some(true) {
            state.path_thinking = true;
        } else if response.get("thinking_enabled").and_then(Value::as_bool) == Some(false) {
            state.path_thinking = false;
        }
        if let Some(fragments) = response.get("fragments").and_then(Value::as_array) {
            for fragment in fragments {
                push_fragment_delta(fragment, false, state, collected, model, &mut deltas);
            }
        }
    }

    if path == "response/fragments" {
        match value {
            Some(Value::Array(fragments)) => {
                for fragment in fragments {
                    push_fragment_delta(fragment, true, state, collected, model, &mut deltas);
                }
            }
            Some(fragment) if fragment.is_object() => {
                push_fragment_delta(fragment, true, state, collected, model, &mut deltas);
            }
            _ => {}
        }
    }

    if path == "response"
        && let Some(entries) = value.and_then(Value::as_array)
    {
        for entry in entries {
            if entry.get("p").and_then(Value::as_str) == Some("response")
                && entry
                    .get("v")
                    .and_then(|v| v.get("thinking_enabled"))
                    .and_then(Value::as_bool)
                    == Some(true)
            {
                state.path_thinking = true;
            }
        }
    }

    if path == "response/search_status" {
        return deltas;
    }

    if path == "response/search_results"
        && let Some(items) = value.and_then(Value::as_array)
    {
        if op != Some("BATCH") {
            state.search_results = items.iter().map(SearchResult::from_value).collect();
        } else {
            static CITE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^(\d+)/cite_index$").expect("static cite-index pattern")
            });
            for item in items {
                let Some(caps) = item
                    .get("p")
                    .and_then(Value::as_str)
                    .and_then(|p| CITE.captures(p))
                else {
                    continue;
                };
                let Ok(index) = caps[1].parse::<usize>() else {
                    continue;
                };
                if let Some(entry) = state.search_results.get_mut(index)
                    && let Some(cite) = item.get("v").and_then(Value::as_i64)
                {
                    entry.cite_index = Some(cite);
                }
            }
        }
        return deltas;
    }

    if let Some(text) = value.and_then(Value::as_str) {
        push_text_delta(text, state, collected, model, &mut deltas);
    } else if path == "response"
        && let Some(entries) = value.and_then(Value::as_array)
    {
        for entry in entries {
            let Some(items) = entry.get("v").and_then(Value::as_array) else {
                continue;
            };
            let joined: String = items
                .iter()
                .map(|item| item.get("content").and_then(Value::as_str).unwrap_or(""))
                .collect();
            if !joined.is_empty() {
                push_text_delta(&joined, state, collected, model, &mut deltas);
            }
        }
    }

    deltas
}

fn push_fragment_delta(
    fragment: &Value,
    set_path_from_type: bool,
    state: &mut FrameState,
    collected: &mut Collected,
    model: &str,
    deltas: &mut Vec<(bool, String)>,
) {
    if set_path_from_type || fragment.get("type").is_some() {
        state.apply_fragment_type(fragment);
    }
    let Some(raw) = fragment.get("content").and_then(Value::as_str) else {
        return;
    };
    if raw.is_empty() {
        return;
    }
    push_text_delta(raw, state, collected, model, deltas);
}

fn push_text_delta(
    raw: &str,
    state: &FrameState,
    collected: &mut Collected,
    model: &str,
    deltas: &mut Vec<(bool, String)>,
) {
    let text = format_stream_content(raw, model);
    if text.is_empty() {
        return;
    }
    if state.path_thinking {
        collected.reasoning.push_str(&text);
        deltas.push((true, text));
    } else {
        collected.content.push_str(&text);
        deltas.push((false, text));
    }
}

/// Read a DeepSeek body into the OpenAI chunk stream.
///
/// `chunks` is the upstream byte stream; the returned stream ends with
/// `data: [DONE]` on every path, including an upstream body that stays open
/// after `FINISHED` (the drain timer forces the close).
pub fn transform_stream(
    chunks: ByteStream,
    model: String,
    stream_id: String,
    created: u64,
) -> ByteStream {
    use futures::StreamExt;

    Box::pin(async_stream::stream! {
        let mut collected = Collected::default();
        let mut state = FrameState::default();
        let mut buffer = String::new();
        let mut emitted_role = false;
        let mut finished = false;
        let mut stream = chunks;

        let chunk_json = |delta: Value, finish: Option<&str>| {
            json!({
                "id": stream_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "system_fingerprint": Value::Null,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish.map_or(Value::Null, |f| json!(f)),
                    "logprobs": Value::Null,
                }],
            })
        };
        let role_chunk = chunk_json(json!({"role": "assistant", "content": ""}), None);

        let drain = tokio::time::sleep(std::time::Duration::from_millis(FINISHED_DRAIN_MS));
        tokio::pin!(drain);
        let mut drain_armed = false;

        loop {
            let chunk = if drain_armed {
                tokio::select! {
                    biased;
                    _ = &mut drain => None,
                    next = stream.next() => next,
                }
            } else {
                stream.next().await
            };

            let Some(chunk) = chunk else {
                break;
            };
            let Ok(bytes) = chunk else {
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].to_string();
                buffer.drain(..=newline);
                let Some(payload) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" || payload.is_empty() {
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };

                let was_finished = collected.saw_finished;
                let deltas = apply_payload(&parsed, &mut state, &mut collected, &model);

                if parsed.get("p").and_then(Value::as_str) == Some("response/status")
                    && parsed.get("v").and_then(Value::as_str) == Some("FINISHED")
                {
                    collected.saw_finished = true;
                }

                for (is_reasoning, text) in deltas {
                    if !emitted_role {
                        emitted_role = true;
                        yield Ok(bytes::Bytes::from(sse_chunk(&role_chunk)));
                    }
                    let delta = if is_reasoning {
                        json!({"reasoning_content": text})
                    } else {
                        json!({"content": text})
                    };
                    yield Ok(bytes::Bytes::from(sse_chunk(&chunk_json(delta, None))));
                }

                if collected.saw_finished && !was_finished {
                    drain_armed = true;
                    drain.as_mut().reset(tokio::time::Instant::now()
                        + std::time::Duration::from_millis(FINISHED_DRAIN_MS));
                } else if collected.saw_finished && drain_armed {
                    // A late payload extends the window, so trailing
                    // `search_results` still land.
                    drain.as_mut().reset(tokio::time::Instant::now()
                        + std::time::Duration::from_millis(FINISHED_DRAIN_MS));
                }

                if collected.saw_finished {
                    finished = true;
                    break;
                }
            }

            if finished && drain_armed {
                // Keep draining until the timer fires, then fall out below.
                continue;
            }
        }

        let citations = append_search_citations(&state.search_results, &model);
        if !citations.is_empty() {
            if !emitted_role {
                emitted_role = true;
                yield Ok(bytes::Bytes::from(sse_chunk(&role_chunk)));
            }
            yield Ok(bytes::Bytes::from(sse_chunk(&chunk_json(
                json!({"content": format!("\n\n{citations}")}),
                None,
            ))));
        }
        if !emitted_role {
            yield Ok(bytes::Bytes::from(sse_chunk(&role_chunk)));
        }
        yield Ok(bytes::Bytes::from(sse_chunk(&chunk_json(json!({}), Some("stop")))));
        yield Ok(bytes::Bytes::from(SSE_DONE));
    })
}

/// Buffer the whole DeepSeek body into one OpenAI `chat.completion` object.
pub async fn collect_completion(chunks: ByteStream) -> Collected {
    use futures::StreamExt;

    let mut collected = Collected::default();
    let mut state = FrameState::default();
    let mut buffer = String::new();
    let mut stream = chunks;

    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else {
            break;
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].to_string();
            buffer.drain(..=newline);
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            apply_payload(&parsed, &mut state, &mut collected, "");
            if parsed.get("p").and_then(Value::as_str) == Some("response/status")
                && parsed.get("v").and_then(Value::as_str) == Some("FINISHED")
            {
                collected.saw_finished = true;
            }
        }
    }

    collected
}

/// The non-streaming `chat.completion` body.
pub fn completion_body(model: &str, collected: &Collected, created: u64) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(collected.content));
    if !collected.reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(collected.reasoning));
    }
    json!({
        "id": format!("chatcmpl-{created}"),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": "stop",
            "logprobs": Value::Null,
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_kind_predicates_match_the_id_wording() {
        assert!(is_thinking_model("deepseek-v4-pro-think"));
        assert!(is_thinking_model("DeepSeek-R1"));
        assert!(is_thinking_model("deepseek-reasoner"));
        assert!(!is_thinking_model("deepseek-chat"));
        assert!(is_search_model("deepseek-v4-pro-search"));
        assert!(!is_search_model("deepseek-chat"));
    }

    #[test]
    fn transport_tokens_are_stripped() {
        assert_eq!(clean_token("FINISHED"), "");
        assert_eq!(clean_token("SEARCH hello"), "hello");
        assert_eq!(clean_token("WEB_SEARCH  grey"), "grey");
        assert_eq!(clean_token("plain"), "plain");
    }

    #[test]
    fn citations_become_brackets_only_on_a_search_model() {
        assert_eq!(
            format_stream_content("a[citation:1]b", "deepseek-v4-pro-search"),
            "a[1]b"
        );
        assert_eq!(
            format_stream_content("a[citation:1]b", "deepseek-chat"),
            "a[citation:1]b"
        );
        assert_eq!(
            format_stream_content("a[citation:1]b", "deepseek-search-silent"),
            "ab"
        );
    }

    #[test]
    fn citations_are_sorted_and_skipped_when_absent() {
        let results = vec![
            SearchResult {
                cite_index: Some(2),
                title: "two".into(),
                url: "u2".into(),
            },
            SearchResult {
                cite_index: None,
                title: "none".into(),
                url: "u0".into(),
            },
            SearchResult {
                cite_index: Some(1),
                title: "one".into(),
                url: "u1".into(),
            },
        ];
        assert_eq!(
            append_search_citations(&results, "deepseek-search"),
            "[1]: [one](u1)\n[2]: [two](u2)"
        );
        assert_eq!(append_search_citations(&[], "deepseek-search"), "");
    }

    #[test]
    fn a_fragment_payload_routes_think_and_answer_by_type() {
        let mut state = FrameState::default();
        let mut collected = Collected::default();
        let mut deltas = Vec::new();

        push_fragment_delta(
            &json!({"type": "THINK", "content": "why"}),
            true,
            &mut state,
            &mut collected,
            "m",
            &mut deltas,
        );
        push_fragment_delta(
            &json!({"type": "ANSWER", "content": "because"}),
            true,
            &mut state,
            &mut collected,
            "m",
            &mut deltas,
        );
        assert_eq!(collected.reasoning, "why");
        assert_eq!(collected.content, "because");
    }

    #[test]
    fn batch_search_results_patch_cite_indexes() {
        let mut state = FrameState::default();
        let mut collected = Collected::default();
        apply_payload(
            &json!({"p": "response/search_results", "v": [
                {"title": "a", "url": "ua"},
                {"title": "b", "url": "ub"},
            ]}),
            &mut state,
            &mut collected,
            "m",
        );
        apply_payload(
            &json!({"p": "response/search_results", "o": "BATCH", "v": [
                {"p": "0/cite_index", "v": 2},
            ]}),
            &mut state,
            &mut collected,
            "m",
        );
        assert_eq!(state.search_results[0].cite_index, Some(2));
        assert_eq!(state.search_results[1].cite_index, None);
    }

    #[test]
    fn an_empty_fragment_type_leaves_the_path_alone() {
        let mut state = FrameState {
            path_thinking: true,
            search_results: Vec::new(),
        };
        state.apply_fragment_type(&json!({"type": "OTHER"}));
        assert!(state.path_thinking);
        state.apply_fragment_type(&json!({"type": "answer"}));
        assert!(!state.path_thinking);
    }
}
