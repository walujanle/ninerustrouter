# OAuth, Credentials, Token Refresh, Account Fallback

Scope: `crates/router-sse/src/services/{oauth_flow,token_refresh,account_fallback,auth,background_token_refresh,single_flight}.rs`, `crates/router-server/src/routes/oauth.rs`, `crates/router-server/src/services/codex_proxy.rs`, `crates/router-server/src/auth/guard.rs`, `crates/router-db/src/repos/connections.rs`.

## What this covers

**4** provider OAuth flows ship: `codex`, `kilocode`, `codebuddy-intl`,
`nous`. Credential storage in `providerConnections.data`,
proactive and reactive token refresh, per-model cooldown, and the
account-selection loop. These authenticate to **upstream AI providers**; the
dashboard login is a separate mechanism.

## Crates

`reqwest` (rustls, form, json), `tokio`, `serde`/`serde_json` (`preserve_order`),
`sha2`, `base64` (`URL_SAFE_NO_PAD` for PKCE/JWT), `rand`, `url`, `thiserror`,
`dashmap`, `uuid`.

## Module layout

```
crates/router-sse/src/services/
  oauth_flow.rs                authorize URL, token exchange, device code, poll
  token_refresh.rs             refresh_with_retry, per-provider refreshers, single-flight
  background_token_refresh.rs  the "is this connection due?" predicate
  account_fallback.rs          error rules, model locks, cooldown maths
  auth.rs                      account selection, mark_account_unavailable, clear_account_error
  single_flight.rs             generic in-flight dedup
crates/router-server/src/
  routes/oauth.rs              the /api/oauth/* actions
  services/codex_proxy.rs      the Codex 1455 callback listener and its session map
  auth/guard.rs                the local-request origin guard
crates/router-db/src/repos/connections.rs   providerConnections read/write
```

## Provider dispatch

Provider config comes from `registry.json`'s top-level `providerOauth` map, never
from a second table in Rust. `oauth_flow.rs` reads it through
`registry().oauth(provider)` and dispatches on the provider id:

- `flow_type(provider)` returns `authorization_code_pkce` (`codex`) or
  `device_code` (`kilocode`, `codebuddy-intl`, `nous`).
- `generate_auth_data` builds the PKCE pair and the authorize URL.
  `exchange_tokens` completes the auth-code flows; `request_device_code` and
  `poll_for_token` drive the device-code flows.
- `build_auth_url` and the per-provider token mappers are match arms on the
  provider id.

`flow_type` is a hand-maintained table because the registry stores config, not
the flow. A new provider needs both a `providerOauth` entry and a match arm.

**Nous is device-code with a second credential.** After the token exchange
succeeds it mints an agent key (`mint_nous_agent_key`) and stores both. The mint
is best-effort: the login itself succeeded, so a failed mint falls back to the
access token rather than failing the flow. Because Nous rotates the OAuth pair
*and* the short-lived agent key, its refresh goes through
`refresh_token_by_provider` — the scheduler's refresher — and not the generic
`grant_type` path the other providers use. That `match` on `provider` in
`default.rs` is the reason a new dual-credential provider is not just a registry
entry.

## Credential storage

`providerConnections.data` is a JSON blob whose key names and null-vs-absent
semantics are load-bearing. The database is shared with 9router and the schema is
byte-identical, so this mapping is a contract: any serde mismatch breaks DB
compatibility.

`crates/router-db/src/repos/connections.rs` owns the mapping:

- `row_to_conn` parses `data` first, then overwrites the fixed columns `id`,
  `provider`, `authType`, `name`, `email`, `priority`, `isActive`, `createdAt`,
  `updatedAt`. A key already present in the blob keeps its original position, so
  the exported JSON is byte-identical.
- `conn_to_row` strips those columns back out and stringifies the rest into
  `data`. `isActive` is `false ? 0 : 1` — anything not literal `false` is 1.

Everything outside the fixed columns is a free-form map, not fixed fields:
cooldown state is stored as dynamic `modelLock_<model>` keys, and model names
contain `/` and `:`.

## Refresh

- `refresh_with_retry` runs 3 attempts with a linear `attempt * 1000ms` delay.
  `classify_oauth_refresh_error` marks a permanent failure, and
  `is_unrecoverable_refresh_error` treats `unrecoverable_refresh_error`,
  `refresh_token_reused`, `invalid_request` and `invalid_grant` as
  unrecoverable, so a permanently-dead refresh token is not retried.
- Dedup: `dedup_refresh` runs a 10-second single-flight window keyed
  `provider:oldToken`. Two concurrent requests with the same token must share one
  in-flight future, or both hit the provider and one invalidates the other's
  rotated token.
- A second lock, keyed by connection rather than token, stops two requests for one
  account from refreshing even when they arrive with different tokens.
- **Rotating single-use refresh tokens.** `codex` issues a new refresh token on
  every refresh, and `refresh_with_retry` mutates the credential handle in place
  between attempts. A clone-per-attempt design re-uses a consumed token and gets
  `invalid_grant`. Mutating in place is required even though the attempt is
  `&mut`, so nothing in the retry loop may take a copy of the token.
- **A connection whose provider has no `providerOauth` entry is never selected for refresh.**
  `background_token_refresh::select_connections_needing_refresh` skips it before the refresher runs,
  so a stale row for a provider with no refresher is inert data the tick stops chasing. Without that
  predicate the row failed on every tick and logged a failed refresh line on every tick.
  `authType` is matched case-insensitively with underscores removed (`to_lowercase().replace('_', "")`),
  so `O_Auth` counts as OAuth and `OAuth_2` does not.
- Background sweep (`crates/router-server/src/services/schedulers.rs`): a
  30-minute default lead (`BACKGROUND_REFRESH_LEAD_MS`, or the provider's own
  larger lead), a 5-minute interval, a 10-second initial delay, and a sequential
  `1.5s + 200ms` inter-account delay so a batch of refreshes does not look like
  credential stuffing.

## Per-provider quirks

- **Claude** aligns `x-claude-code-session-id` with `metadata.user_id.session_id`
  when the client sends no such header and the token carries `sk-ant-oat`. The
  `sk-ant-oat` gate keeps this to OAuth connections; an API-key connection is left
  alone.
- **Codex** refreshes proactively on a stale stamp: `maxRefreshAgeMs` is 8 days
  and `refreshLeadMs` is 5 days, both read from its registry entry. It also has a
  bulk-import path.
- **NVIDIA NIM** treats `reasoning` and `reasoning_content` as aliases for one field
  and fails deserialization on a message carrying both. The `nvidia` rule in
  `translator/concerns/param_support.rs` renames `reasoning` to
  `reasoning_content` on assistant turns (before the drop list runs, and without
  overwriting a value the client already sent). A blank string counts as absent,
  so the alias fills it.
- **CodeBuddy** reports its frequency limit as HTTP 400 with body code `6004`.
  `CodeBuddyIntlExecutor::parse_error` maps it to `429` and extracts the reset
  instant from the message text as `resetsAtMs`, so the account cools for the
  right window instead of a generic one. The message carries a local
  `YYYY-MM-DD HH:MM:SS` with an optional `UTC+8`-style suffix; with no suffix the
  gateway's own zone (UTC+8) is assumed. An unparseable message yields no
  `resetsAtMs` at all rather than an invented time.
- **CodeBuddy** posts the literal `{}` (two bytes) to its billing endpoint
  (`/v2/billing/meter/get-user-resource`). The endpoint answers 400 to a zero-byte
  body, so the request is a JSON POST with a body, not an empty one.
- **Codex** is the only usage handler that propagates an error instead of
  returning a `{message}` payload, and the only one that merges three
  rate-limit key shapes into one object.

## Loopback origin guard

Legit OAuth redirects send **no** `Origin` header.
`crates/router-server/src/auth/guard.rs` allows an absent `Origin` and rejects a
non-loopback one, which blocks login-CSRF without breaking the redirect.
`is_loopback_hostname` handles IPv6 and `::ffff:` forms.

## Import paths

The `/api/oauth/*` routes carry two import flows: `POST
/api/oauth/codex/bulk-import` and `POST /api/oauth/codex/import-token`. Each
writes a connection row through the same repo as a normal connect. The `exchange`
action also accepts a raw JWT pasted in place of a code and stores it as an
`access_token` connection.

## Browser launch

The frontend opens the auth URL (`window.open` in
`web/src/components/OAuthModal.vue`); the backend only supplies it.
`web/src/views/CallbackPage.vue` relays the code: `postMessage` to an allowlist
(`window.location.origin` and `http://localhost:1455`), then `BroadcastChannel
'oauth_callback'`, then a `localStorage` fallback. The Codex path depends on the
`1455` listener in `crates/router-server/src/services/codex_proxy.rs`; if it
breaks, the popup closes with no error.

## Account selection

`get_provider_credentials(provider, excludeConnectionIds, model)` in
`crates/router-sse/src/services/auth.rs` returns `AccountSelection::Selected`,
`AllRateLimited` or `None`. The loop in `crates/router-sse/src/handlers/chat.rs`:

1. Get credentials for the next candidate, excluding already-failed connection ids.
2. `check_and_refresh_token`.
3. Call the chat core.
4. On failure: `mark_account_unavailable(...).should_fallback` decides.
5. On fallback: add the connection id to the exclusion set, record the error, loop.
6. On success: `clear_account_error`.

`mark_account_unavailable` writes a per-model cooldown as `modelLock_<model>` with
exponential backoff (`BACKOFF_BASE_MS` 2s, doubling, capped at `BACKOFF_MAX_MS` of
5 minutes), classified by `ERROR_RULES`. A provider-reported rate-limit reset is
capped at `MAX_RATE_LIMIT_COOLDOWN_MS` (30 minutes).

An unclassified client error — a 400/405/406/409/413/422 that matched no
`ERROR_RULES` entry — answers `should_fallback: false` **and**
`request_scoped: true`. The credential is fine; this member cannot serve this
request. The account stays in rotation, and a combo advances to the next member
instead of aborting.

`clear_account_error` resets `testStatus`/`lastError` **only when no active locks
remain**, clears the succeeded model's lock, and drops expired locks. Getting this
wrong permanently disables an account.

A status generated at the relay hop is not the provider's. A `520`–`527` from the
Vercel relay is Cloudflare's edge error for the worker, so `is_relay_edge_error`
returns it as-is and skips `mark_account_unavailable` — the credential is healthy
and must not be cooled or rotated out. A provider's own 4xx/5xx, `429` included,
still cools. The relay worker asks the origin for `accept-encoding: identity` and
strips `content-encoding`, `content-length`, `transfer-encoding` and `connection`
before rebuilding the response, so the body and its framing agree and the edge
does not answer 520 for a request the provider never saw.

## Quota reads

`parse_reset_time` in `crates/router-sse/src/services/usage.rs` normalises the
reset stamps every provider sends. A falsy value yields nothing; a number or
all-digit string below `1e12` is read as seconds and at or above as milliseconds;
anything else is parsed as RFC-3339 first, then as a bare `YYYY-MM-DD` at UTC
midnight. The two zeroes diverge on purpose: numeric `0` is missing, the string
`"0"` is the epoch.

Strict RFC-3339 is not enough. The parser also accepts what JS `new Date()`
takes and RFC-3339 rejects, through `normalize_reset_datetime`: a
space-separated `YYYY-MM-DD HH:MM:SS`, a `T`-form with no offset, a colonless
`+HHMM` offset, and a `UTC+8` / `GMT+8` zone token. CodeBuddy's billing endpoint
emits the space-separated form, which is why a strict parser showed its reset
time as `N/A` rather than as a wrong value — the field simply never parsed.

## Combo state

`handle_combo_chat` and `handle_fusion_chat` keep rotation state in a
process-global map (`COMBO_ROTATION_STATE`) keyed by combo name. Not persisted,
lost on restart — acceptable, but note it.

`detect_required_capabilities` + `augment_models_with_capacity_adapter` +
`strip_for_adapter_model` add a capability-bearing model to the candidate list and
strip the adapter-only fields on the way out.

## Open decisions

- `flow_type` is hand-maintained while provider config is registry-driven. Whether
  to generate the flow table from `registry.json` is open.
- Combo and fusion rotation state is in-process. Whether it should survive a
  restart is open.

## Auto quota tracker

The tracker in `crates/router-db/src/repos/quota_tracker.rs` reuses the
account-selection machinery: a connection whose quota is exhausted is disabled the
same way a manual bulk "Turn off Empty" would, and re-enabled the same way a
"Turn on Available" would. It is off unless `settings.quotaAutoTrackerEnabled` is
true. See `RUNTIME.md` for the rules.
