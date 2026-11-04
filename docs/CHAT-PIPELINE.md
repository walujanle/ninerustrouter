# Chat Pipeline

The engine in `crates/router-sse/src/`: detect the client wire format, translate the request to the
provider's format, call the provider, translate the response back, stream it.
`handlers/chat_core.rs::handle_chat_core` is the orchestrator; `handlers/chat.rs::handle_chat` is the
app-side entry that authenticates, resolves an account and expands combos. The crate is HTTP-free, so
the pipeline returns a status, a header list and a body; `crates/router-server/src/routes/v1.rs`
turns that into an axum response.

Two constraints shape it: it serves on port 20129, and the database is shared with 9router and the
schema is byte-identical, so a row ninerustrouter does not recognise is left untouched.

## Stage order in `handle_chat_core`

Order is load-bearing. Every step below assumes the previous one ran.

1. Provider thinking injection.
2. `TOKEN_SAVER_HEADER` opt-out check.
3. Stream determination: the transport's `force_stream` wins, then the client's `stream` flag and
   `accept` header decide.
4. `detect_client_tool`.
5. Native passthrough check (`is_native_passthrough`).
6. `strip_unsupported_modalities`.
7. `prefetch_remote_images`.
8. `translate_request` — see the translator registry below for what it runs.
9. `strip_continuity_fields` (`encrypted_content`, `reasoning_encrypted_content`).

11. `strip_unsupported_params` (`translator/concerns/param_support.rs`) — a rule table, not scattered
   deletes. A rule may `drop` top-level params, `drop_message_fields` from assistant turns, or
   `rename_message_fields` on them (applied before the drops, and never overwriting a value the
   client already sent). The nvidia rule renames `reasoning` to `reasoning_content`, because NIM
   treats the two names as one field and rejects a message carrying both.
11. Token savers: RTK, Caveman, Ponytail.
12. `anchor_claude_cache`.
13. `executor.execute`.
14. On 401/403: `refresh_with_retry`, then retry once, mutating rotated refresh tokens **in place**.
15. `parse_upstream_error` on failure.
16. Dispatch: forced-SSE-to-JSON / non-streaming / streaming.

## Translator registry

`translator/mod.rs` owns `Registry`, a string-keyed map with two `HashMap`s keyed `"from:to"`.
`register(from, to, req_fn, res_fn)` takes either function; `register_all` lists every pair in one
place, so there is no import-order hazard and a translator that is not registered there never runs.
The process-wide registry is built once into a `LazyLock` by `registry()`.

**The engine pivots through OpenAI as the intermediate format.** `translate_request` and
`translate_response` look up the exact `source:target` pair first and run it as a **direct route**,
skipping the lossy double hop; only when no pair is registered does the pivot run — source → OpenAI,
then OpenAI → target. Prefer a direct route for fragile pairs: thinking blocks, tool ids, non-base64
images, `is_error`.

The registered pairs:

- requests — `claude:openai`, `openai:claude`, `openai-responses:openai`, `openai:openai-responses`,
  `openai:commandcode`
- responses — `claude:openai`, `openai:claude`, `openai:openai-responses`,
  `openai-responses:openai`, `commandcode:openai`

`RequestFn` is a plain `fn` pointer (`fn(&mut RequestContext<'_>, Value) -> Value`) and `ResponseFn`
returns `Vec<Value>`; translators carry no state, so trait objects would be the wrong shape.

`translate_request` runs: `strip_content_types`, `normalize_thinking_config`, `ensure_tool_call_ids`,
`fix_missing_tool_responses`, `capture_thinking`, `resolve_session_id`, `apply_thinking`,
`filter_to_openai_format`, and `prepare_claude_request`.

`apply_thinking` carries per-provider quirks that must not be flattened (`thinking.rs`). The `zai`
arm disables reasoning with `enable_thinking: false`, because Z.ai ignores `thinking.disabled`, and
sends `reasoning_effort` only when the model's capability row reports the effort is supported. The
Gemini arms raise `maxOutputTokens` to a floor keyed to the budget or level, capped by the model's
max output, so the thinking budget fits.

`translate_response` has a **null-chunk flush contract**: a translator may return an empty `Vec` (or
a `Value::Null` item) to mean "nothing to emit yet", and the stream must not treat that as
end-of-stream. Get this wrong and SSE streams truncate intermittently.

`ResponseState` carries per-request translator state, passed as `&mut ResponseState` alongside the
chunk. Anything a translator invents beyond the typed fields lives in `ResponseState::extra`, so the
state struct does not need editing for every new translator.

## Executors

`executors/mod.rs` holds the special-executor map: `Arc<dyn Executor>` entries for the providers that
need custom code — `codex`, `commandcode`, `opencode`, `opencode-go`, `codebuddy-intl` and
`deepseek-web`. `get_executor` falls back to a per-provider
cached `DefaultExecutor`, which serves every OpenAI-compatible provider.

The `Executor` trait's default methods provide the URL fallback loop, per-status retry with backoff,
connect-timeout abort, and the `should_retry` / `parse_error` / `refresh_credentials` /
`compute_retry_delay` hooks. `execute` owns the loop and the `CancellationToken` select.

`DefaultExecutor` provides: registry-driven auth descriptors, OpenAI/Anthropic-compatible URL
building, `anthropic-beta` selection, generic OAuth refresh grants.

The `Anthropic-Beta` header **merges** the selected flag list with any `anthropic-beta` the client
sent (`merge_anthropic_beta` in `providers/shared.rs`); it never overwrites the client's. A client
carrying a beta the selector does not know must keep it on the request, so a `claude` /
`anthropic-compatible-*` call writes the union, deduplicated and trimmed.

Claude OAuth aligns `x-claude-code-session-id` with the session the client already announced. When
the header is absent and the bearer token is an `sk-ant-oat` token, `DefaultExecutor` derives it from
`metadata.user_id.session_id` (`session_manager.rs::extract_claude_code_session`), accepting either a
JSON `{session_id}` object or a plain string with a leading `claude:` stripped. The `sk-ant-oat` gate
keeps this to OAuth connections.

Binary upstreams decode **inside their own executor** and emit OpenAI-shaped SSE. They never go
through the translator:

- **CommandCode**: AI-SDK v5 NDJSON → OpenAI `chat.completion.chunk` SSE, re-framed by the executor.
  An upstream `error` event emits an error frame and ends the stream, matching the openai-responses
  translator. A generator that panicked here instead dropped the connection, so the client saw a
  truncated stream with no reason.

**CodeBuddy Intl sends the plugin's channel headers.** `www.codebuddy.ai` runs a per-request
channel check and rejects a call without the official plugin's identifiers — HTTP 400, code 11128
"request illegal", which never reaches the model. The transport declares
`auth.hooks: ["codebuddyHeaders"]`, and `apply_header_hooks` (`executors/default.rs`) implements the
hook: one fresh `X-Request-ID` reused as `X-Conversation-Request-ID` and `X-Conversation-Message-ID`,
plus an independent `X-Conversation-ID`. The header set has one definition, in the hook, so the
executor carries no second copy.

The hook runs **before** auth, which matters: a hook that ran after would be free to clobber the
bearer token. Adding a hook means adding a match arm *and* the registry key — a registry `hooks`
entry with no arm is a silent no-op.

**`deepseek-web` is a browser-session provider with a proof-of-work gate.** chat.deepseek.com's web
API takes a flat `prompt` string guarded by an `X-Ds-Pow-Response` header, so `DeepSeekWebExecutor`
overrides `execute` end to end rather than any one hook: `userToken` to access token
(`/api/v0/users/current`), then chat session (`/chat_session/create`), then PoW challenge
(`/chat/create_pow_challenge`), then completion. Both the access token and the session are cached by
`userToken`, and the session cache is skipped unless the connection sets `persistSession`.

The PoW is DeepSeekHashV1 — SHA3-256 with round 0 of the Keccak-p[1600] permutation skipped, so 23
rounds where FIPS-202 uses 24. `executors/deepseek_web/pow.rs` hand-ports the permutation because no
crate in the tree exposes a configurable round count. The solver walks `0..difficulty` and returns
`None` when the space is exhausted; difficulty is validated against upstream's own ceiling
(`MAX_DEEPSEEK_POW_DIFFICULTY`, 250000) before the solve.

**`DefaultExecutor` builds every outbound client through `http::tls_builder`.** A `reqwest` client
built directly would take reqwest's default verifier, which panics on Termux (see `RUNTIME.md`).
The same applies to credential-refresh and OAuth calls: those are bounded (a 30 s budget for OAuth,
the streaming first-chunk budget for a non-streaming body read) so a stalled upstream surfaces a
gateway timeout instead of holding the task open forever.

## Streaming

`utils/stream.rs` owns `SseStream`, with two modes:

- **Passthrough** (source format == target format): normalise chunks, extract usage.
- **Translate**: every upstream chunk through `translate_response`, plus the null-chunk flush,
  Responses terminal synthesis, and `[DONE]` emission.

`utils/stream_handler.rs` owns `pipe_with_disconnect` — abort, disconnect detection, the stall
watchdog and the idle keepalive frame.

**`SseStream::push` splits lines in place.** It used to collect the whole buffer into a fresh
`Vec<String>` and rebuild it from the remainder, allocating one `String` per line plus a new buffer on
every chunk; `decode` separately allocated a `String` that was appended to that buffer. It now takes
the buffer out, borrows each complete line in place, and drains what it consumed, leaving only the
partial tail. The splitter's behaviour is pinned by a test that feeds the same frame at every
possible chunk boundary, which is the only honest way to prove an in-place rewrite is equivalent.

**The stall watchdog must be tied to raw upstream bytes, not transform output.** If no upstream byte
arrives within `stall_timeout_ms`, the stream is dead. Watching the transform output instead makes a
slow translator look like a stalled provider.

**Idle keepalive.** The router's own stall budget is far longer than some clients'. Claude Code
aborts an SSE connection after roughly 10 s with no bytes, so a slow first token or a long reasoning
pause surfaced as a truncated stream and a fallback retry. After `SSE_KEEPALIVE_INTERVAL_MS` (5 s)
of upstream silence `stream_handler.rs` writes `: keepalive`, an SSE comment frame every parser
ignores. The frame needs no knowledge of the client's format, and setting the interval to `0`
disables it.

On abort, terminal bytes are written before closing; a completed stream is never followed by a
synthetic error frame.

Rust mechanics: the transform is an `Fn(ByteStream) -> ByteStream`; `routes/v1.rs` wraps the result in
`Body::from_stream`. A `CancellationToken` is the abort signal, and the watchdog task is what enforces
the stall timer.

**Usage estimation is bounded work, not accumulation.** `build_transform_stream`
(`chat_core/streaming.rs`) estimates input tokens once from the request body when the stream is
built, so a full request `Value` is not held for the stream's life. The transform counts emitted
content and thinking length — in UTF-16 code units, matching JS `String.length` — instead of
retaining the text, and derives the output-token estimate (`length / 4`, minimum 1) when the upstream
sends no usage.

`handlers/chat_core/streaming.rs` also guards against a non-SSE upstream body (HTML error page) by
converting it to a sanitised JSON error.

`build_on_stream_complete` persists the final usage row and logs the done line.

## Non-streaming and forced-SSE-to-JSON

`handlers/chat_core/non_streaming.rs` holds hand-written whole-body response converters for Claude /
Responses → OpenAI, used when no registry entry exists — the registry's response translators
are streaming-shaped, and a non-streaming body needs one whole-body conversion instead.

`handlers/chat_core/sse_to_json.rs` handles the case where the provider forces streaming but the
client wants JSON: `parse_sse_to_openai_response` plus the Responses → Chat conversion. The branch
keys on the *upstream* format, not the client's.

`handlers/chat_core/responses_convert.rs` holds the Chat Completions → Responses converter that both
the non-streaming and forced-SSE handlers call (`completion_to_responses`). The two callers differ
only in the terminal `status`: `None` derives it from `finish_reason`, `Some("completed")` pins it.
One module holds the logic because a third module both siblings depend on has no import cycle, so two
near-identical copies are not needed.

`parse_sse_to_openai_response` always yields a `chat.completion`, so a forced-SSE request whose
client is Claude must be re-shaped with `open_ai_completion_to_claude_message` before it is returned.
Without that step a `/v1/messages` request comes back as HTTP 200 with an OpenAI `{"choices":[…]}`,
which Claude Code rejects as a malformed response. Both forced-SSE branches — the standard one and
the non-Responses tail of the Codex path — convert when the client format is Claude, through the same
helper the normal non-streaming path uses.

## Data model

Keep `serde_json::Value` as the hub format. The formats are lossy, provider-shaped, and mutated in
place; typed structs would mean a struct per wire format plus every provider quirk, and the pipeline
relies on unknown-field passthrough (`extend_fields`, vendor reasoning shapes). Type only the binary
boundaries.

Whole-valued floats serialize without a decimal point. `serde_json` writes an `f64` `35.0` where
`JSON.stringify(35)` writes `35`, and payloads here are byte-compared, so every whole number that
reaches a response goes through `js_json_number` (`translator/concerns/primitives.rs`). The
thinking-budget emission sites (`thinking.rs`) and the usage/quota builders are the callers in this
pipeline.

Private side channels stay out of the wire `Value`. `RequestMeta` carries `_toolNameMap` and
`_customToolNames`, and `ResponseState` carries its own scratch — if those keys were smuggled into the
body they would leak into the upstream request.

**Session identity.** `session_manager.rs` resolves the per-request session id from the client's
headers or body, falling back to an id derived from the connection. Three process-global stores
(runtime, assistant-text, continuation) are `LazyLock<Mutex<HashMap>>` with lazy TTL eviction on read
rather than a timer thread, a 2-hour TTL (`SESSION_TTL_MS`) and hard caps of 1000 / 5000 / 5000
entries. The generated id is `randomUUID() + Date.now()`, scoped per connection.

## Format detection

`translator/formats.rs` defines the format identifiers as `&str` constants rather than an enum,
because the same strings are registry keys (`"claude:openai"`) and model `targetFormat` values that
arrive as data; an enum would need a fallible parse at every boundary. The set is closed and is the
wire vocabulary the shared `concerns/`, `thinking` and `providers/service.rs` modules branch on. The
transports in this build declare `openai`, `openai-responses`, `claude` and `commandcode`.

`detect_format_by_endpoint(pathname, body)` picks the source format from the endpoint plus body shape:
`/v1/responses` → `openai-responses`, `/v1/messages` → `claude`, and a `/v1/chat/completions` body
carrying an `input[]` array → `openai`. `get_target_format` picks the provider's format;
`resolve_transport` prefers a multi-endpoint transport whose format matches the source, to avoid
translating at all.

A model may declare a `supportedFormats` list, which limits which transport applies. When the list is
present, `handle_chat_core` uses the runtime transport only if the list contains the source format;
otherwise it drops the transport and translates to the model's target format. This stops a
Claude-format request from routing an OpenAI-only model to `/messages`.

## Known translator limitations

The converters are deliberately lossy in a few places. A new maintainer should know these before
changing a translator:

| Limitation | Owner |
|---|---|
| A Claude image converts only when `source.type` is `base64`; a `url` source is skipped | `translator/request/claude_to_openai.rs` |
| A Claude `thinking` / `redacted_thinking` block has no conversion arm | `translator/request/claude_to_openai.rs` |
| `is_error` on a `tool_result` block is not carried into the OpenAI tool message | `translator/request/claude_to_openai.rs` |
| A `tool_result` image is moved to the following user turn — the OpenAI tool role is text-only | `translator/request/claude_to_openai.rs` |
| OpenAI→Claude always injects the "You are Claude Code" system prompt | `translator/request/openai_to_claude.rs`, `constants.rs` |
| `tool_choice:"none"` becomes `auto` | `translator/request/openai_to_claude.rs` |
| An `input_audio` part has no conversion arm | `translator/request/openai_to_claude.rs` |
| A Responses `function_call` with an empty name is skipped, which can leave an assistant turn with `tool_calls: []` | `translator/request/openai_responses.rs` |
| A Responses `input_image` falls back to using `file_id` as the image URL | `translator/request/openai_responses.rs` |
| `deepseek-web` does not translate tool calls; a request carrying `tools[]` is refused with an explicit `400` | `executors/deepseek_web/mod.rs` |

**Changing one of these is a behaviour change, not a cleanup.** Give it its own changelog entry and a
test that pins the new output, so a future reader can tell an intentional fix from a regression.

## Account selection and fallback

`handlers/chat.rs` is the app-side entry: parse body, strip the 1M-context marker, expand combos, loop
over accounts.

- `get_provider_credentials(db, provider, exclude_connection_ids, model, preferred_connection_id)`
  returns an `AccountSelection` — `Selected`, `AllRateLimited` or `None` (`services/auth.rs`).
- `check_and_refresh_token` refreshes before the call.
- On failure: `mark_account_unavailable(db, connection_id, status, error_text, provider, model,
  resets_at_ms)` returns `should_fallback`. Per-model cooldown is stored as `modelLock_<model>` in the
  connection's `data` blob, with exponential backoff (`BACKOFF_BASE_MS` 2s doubling, capped at
  `BACKOFF_MAX_MS` 5 min and `BACKOFF_MAX_LEVEL` 15) and a provider-reported rate-limit cooldown
  capped at `MAX_RATE_LIMIT_COOLDOWN_MS` (30 min). `ERROR_RULES` in `services/account_fallback.rs`
  classifies statuses and error text, text rules first. An unclassified client error
  (400/405/406/409/413/422) comes back `should_fallback: false, request_scoped: true`: the account is
  healthy and stays in rotation, and a combo advances to the next member rather than ending. Combos
  return the first such member's own status and message when no member can serve the request.
- `clear_account_error` on success resets `testStatus`/`lastError` **only when no active locks
  remain**, clears the succeeded model's lock, and drops expired locks. Getting this wrong permanently
  disables an account.

A relay-generated status never cools the account. When a request goes through the Vercel relay
(`proxy_options.vercel_relay_url` is set) and the response carries a `520`–`527`,
`is_relay_edge_error` returns true and `handle_single_model_chat_inner` skips
`mark_account_unavailable`, returning the status as-is: Cloudflare's edge generated it, the provider
never saw the request, and a healthy credential must not leave rotation. Direct provider 5xx
handling, including the 429 URL fallback, is unchanged.

Combos: `handle_combo_chat` (fallback / round-robin with sticky limit) and `handle_fusion_chat`
(fan-out to N models with quorum grace plus a judge pass), both in `services/combo.rs`. Strategy comes
from per-combo `comboStrategies[name].fallbackStrategy`, else the global `comboStrategy`, defaulting
to `fallback`. Rotation state is a process-global map — not persisted, lost on restart.

**A cycle is rejected while expanding, not discovered by exhausting the stack.** Expansion carries
the chain of combo names being resolved; a name already in the chain answers 400 `Combo cycle:
a -> b -> a`, and a chain longer than `MAX_COMBO_DEPTH` (16) answers `Combo nesting exceeds 16
levels`. Without the chain, a combo naming itself re-entered expansion until the stack ran out — the
abort surfaced as `thread 'tokio-rt-worker' (...) has overflowed its stack`, and raising the stack
size only moved the failure. `POST`/`PUT /api/combos` also reject a cycle at save time, so the stored
graph never holds one. The 16-level cap is for a deep but acyclic nest, which is legitimate.

**A 2xx with nothing in it does not win the combo.** `isZeroContentResponse` (`services/combo.rs`)
treats a response with no assistant text, no tool call and no reasoning as a non-answer, so the next
model gets a turn instead — unless this was the last seat, where the empty response is returned
rather than nothing at all.

**The runtime stack is pinned to 8 MiB.** `server_runtime` in `main.rs` sets it explicitly
(`RUNTIME_STACK_SIZE`). Tokio otherwise leaves the choice to `std::thread`, which uses 2 MiB on both
Unix and Windows. This is headroom for the recursive JSON walkers, not a fix for the cycle above.

Capacity adapter: `detect_required_capabilities(body)` then `augment_models_with_capacity_adapter`
appends models that have the missing capability, with `strip_for_adapter_model` removing the
adapter-only fields on the way out (`services/capacity_adapter.rs`).

Bypass handling runs **before** combo rotation so naming/warmup requests do not consume rotation slots.

Claude Code marks a 1M-context request as `<model>[1m]`; `strip_model_context_marker` removes the
marker before resolution (`utils/model_markers.rs`).

## Token savers

All fail-open: any error returns `None` and leaves the body untouched, and none may throw out of the
hook.

- **RTK** (`rtk/`): compresses `tool_result` content in place. Skips results flagged as errors
  (`is_error`), so error traces survive.
- **Caveman**: level from `cavemanLevel`; `lite` / `full` / `ultra`.
- **Ponytail**: level from `ponytailLevel`; `lite` / `full` / `ultra`.

## CLI fingerprint headers

Providers that gate on client identity need a plausible CLI fingerprint. Each provider's header
block lives in `providers/registry.json` — Codex's `originator` and User-Agent, commandcode's CLI
headers, and the `Anthropic-Version` / `Anthropic-Beta` pair on the `claude` transports.

Two commandcode values are **not** registry constants and are computed per request in
`executors/commandcode.rs`: `x-project-slug` (a slug of the server's own cwd, standing in for the CLI
workspace the gateway does not have) and `x-session-id`, which is read from the **transformed body's
`threadId`** so the header and the body name one session. A fresh id is minted only when the body
carries none. `providers/shared.rs` holds the shared constants (`ANTHROPIC_API_VERSION`, the
beta list and `select_anthropic_beta`); the per-transport header values are the registry's, and are
written verbatim.

### User header overrides

Any of those headers can be replaced from the dashboard without a rebuild. The store is
`settings.providerOverrides`, keyed by canonical provider id and holding `{ headers: {..} }` per
provider — the same key 9router declares, so it is shared settings data and takes **no migration**
(`docs/DB-PARITY.md`).

Three pieces:

- `providers/overrides.rs` validates a payload (`MAX_HEADERS`, the name/value rules, and the
  `BLOCKED_HEADERS` set that refuses `authorization`, `cookie`, `content-type` and the other
  request-structure names) and exposes `builtin_headers(id)` — the registry `transport.headers` the
  UI diffs against and restores to.
- `services/auth.rs` reads the provider's slice off the shared settings snapshot when it builds the
  account's `Credentials.header_overrides`, so a failed settings read degrades to the built-in
  headers rather than failing account selection.
- `executors/executor.rs` calls `apply_header_overrides` **after** `build_headers`, so the override
  wins over the registry values, the hook-generated ids and auth. Last-wins is only safe because the
  blocked list keeps the override off the credential headers.

`GET`/`PUT /api/providers/{id}/overrides` are the dashboard surface. An empty `headers` object
deletes the provider's entry, which is what "restore defaults" sends.
