# Testing Strategy

ninerustrouter's tests are Rust unit tests, colocated with the code in `#[cfg(test)] mod tests` blocks and run with `cargo test --workspace`. There is no committed snapshot corpus and no `tests/` integration crate. A test builds the input it needs: `serde_json::json!` for payloads, an in-memory SQLite connection or a `tempfile::TempDir` for storage.

## Test inventory

About 1,185 test functions live in 172 files carrying a `#[cfg(test)]` module:

| Crate | `#[test]` | `#[tokio::test]` |
|---|---|---|
| `router-db` | 169 | 0 |
| `router-sse` | 801 | 50 |
| `router-server` | 133 | 12 |
| `ninerustrouter` (bin) | 20 | 0 |

Counts are `grep -ro '#\[test\]'` / `#\[tokio::test\]` over `crates/`; `cargo test
--workspace` prints the same totals per crate. They drift as tests are added, so
treat the phrase as the anchor and the table as the shape, not a contract.

The async tests cover the axum handlers. A route test assembles the real `Router` and drives it with `tower::ServiceExt::oneshot`, so the route table is exercised, not just the handler function. `router-db` tests open an in-memory connection and apply the real `PRAGMA_SQL`.

The route table is also asserted by construction: `Router::route` panics when a path and method are registered twice, and that panic fires only when the router is assembled, so a unit test on a handler never catches it. `app::tests::router_builds_without_duplicate_routes` builds the real `Router` from an `AppState` over a scratch database and fails CI if two registrations collide.

Two modules named `*_test.rs` are production code, not test files: `router-server/src/routes/models_test.rs` (the `/api/models/test` route) and `router-sse/src/services/proxy_test.rs` (the proxy reachability check behind `/api/settings/proxy-test`).

## What the tests read

Two committed JSON files are embedded in the binary with `include_str!` and parsed once into `LazyLock` statics: `router-sse/src/providers/registry.json` (the provider registry, the source of truth) and `router-sse/src/catalog/catalog.json` (pricing and model metadata). Tests assert against the parsed data, not a separate fixture file.

## Three layers

### 1. Golden fixtures

Assert the exact bytes the code emits.

- `json_col::stringify_json` is the byte-level JSON writer; its tests pin insertion order, `null` handling, and the rule that a missing value becomes `null` with the key kept.
- `schema` tests pin the literal `CREATE TABLE` text for `settings`, `kv` and `usageHistory`.
- `time` tests pin the RFC 3339 millisecond form (`2026-01-01T00:00:00.000Z`).
- `primitives::js_json_number` writes a finite whole number as an integer. `serde_json` renders the `f64` `35.0` as `35.0`, where `JSON.stringify(35)` writes `35`, so every whole-valued number that is byte-compared against the client (quota bars, percentages, thinking budgets, embedding dimensions) goes through this helper rather than `json!`.

**Requires `preserve_order`.** Every crate enables the `serde_json` feature, because key insertion order and the `null`-versus-omitted distinction are part of the wire and storage contract. A test that compares JSON compares bytes, not values.

### 2. DB round trip

The database is shared with 9router and the schema is byte-identical, so a round trip proves the file is readable by both installs. This is not a value assertion, it is an actual round trip:

1. Create a fresh DB.
2. Write through ninerustrouter: a settings update, a connection upsert, a combo, an API key, a usage record.
3. Reopen it and confirm the same `schemaVersion`, with every value read back.
4. `sqlite3 <file> .schema` from both installs must be identical after step 1 and again after step 2.
5. Compare the raw TEXT of one settings row and one connection row written by each side. **Byte equality, not JSON equality.**

Step 5 is the one that catches key ordering and date precision; steps 1 to 4 pass with a sorted-key serializer and a second-precision clock.

The in-crate tests cover the parts that need no second process: `driver` tests assert the PRAGMAs (`journal_mode = wal`, `busy_timeout = 5000`), transaction rollback, commit, and pool reuse; `migrations` tests assert `ensure_schema` creates every table, stamps `schemaVersion` and is idempotent; every repo round-trips its own rows. Opening a real shared file from two processes is the manual step.

### 3. Differential harness

The way to catch a translation regression end to end: record real request bodies, replay them, and diff the upstream request ninerustrouter produces and the SSE frame sequence it returns against the recording. The `/api/translator/*` debug routes (`console-logs`, `load`, `save`, `send`, `translate`) record and replay a single translation and can seed the corpus.

This is not automated today. The translator tests in `router-sse` cover the individual format pairs.

### 4. Pinned cross-implementation vectors

Where a function only has to agree with an implementation outside this repo, the
test pins vectors produced by that implementation rather than by a second copy of
the same logic here.

The DeepSeek proof-of-work is the case that needed it:
`executors/deepseek_web/pow.rs` is a port of upstream's `deepseek-pow-hash.js`, and
the two agreed only by construction. Five digests and five solved nonces were
generated from upstream's own `findDeepSeekPowNonce` and are asserted in
`pow::tests`. The first port was wrong — the theta step dropped the low word's
carry — and the vectors caught it before a live request could.

A vector test earns its place only when the reference is genuinely external. Two
copies of the same Rust function prove nothing about either.

## Known-bugs decision

A bug that is confirmed but not fixed in the same change stays visible in the suite. Either the test asserts the correct behaviour and the fix lands with it, or the test is `#[ignore = "..."]` with the reason. Hiding a bug by deleting its test is not an option, and every fix updates the `docs/` page that owns the area, like any other behaviour change.

## Test isolation

Tests run in parallel in one process, so two kinds of shared state need care.

A test that opens the database gets its own data directory — `router-server` builds one under the OS temp dir keyed by process id and label — because SQLite refuses two writers on one file.

A test that mutates a process-global store serializes with the other tests that touch it. One such store is cleared between cases: the combo rotation cursor (`rotation_guard` in `services/combo.rs`). The guard is a process-wide `Mutex` whose lock recovers from poisoning, so one failing test does not cascade into false failures in the rest.

## The gate

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo audit
node scripts/set-version.mjs --check

cd web && npm run build              # vue-tsc -b && biome check && vite build
cd web && ./node_modules/.bin/biome ci
cd web && npm test                   # node --test test/*.test.ts
cd web && npm audit --audit-level=high
```

CI runs these on every push to `main` and every pull request
(`.github/workflows/ci.yml`), and the release workflow runs the same gate on the
tag before it builds any artifact (`.github/workflows/release.yml`), so a
published binary is never ahead of the checks. `cargo audit` and `npm audit`
fail the build rather than warning. Call the local Biome binary: `npx biome ci`
resolves `ci` as a package name and fails.

## Registry registration

`crates/router-sse/src/translator/mod.rs` builds the translator registry from one `register_all()` that lists every `(from, to)` pair in a single place. `registry()` wraps it in a `LazyLock`, so it is built once per process and cannot be half-initialised.

A translator that is not registered never runs, and the failure is silent: translation is skipped and the request goes out untranslated. Three tests guard the invariant.

- `every_kept_pair_is_registered` asserts every pair resolves to a function.
- `registry_lookup_respects_pair_order` pins the key as the exact ordered pair, so `claude→openai` and `openai→claude` are separate registrations.
- `unregistered_provider_pairs_return_none` pins that an unknown pair returns `None` rather than falling back to a bridge.

Add a pair by editing `register_all`. Do not introduce an import that registers on a side effect.

## Real-provider tests

Live-provider tests make network calls and are opt-in, never part of required CI. Gate them behind an environment variable and mark them `#[ignore]` so `cargo test --workspace` skips them by default. They read active connections from the shared database under the 9router data directory (`~/.9router`, or `%APPDATA%\9router` on Windows) and treat 401, 402, 403 and 429 as credential problems to skip, not failures.

No such test ships today; the executor tests use local inputs.

## Coverage gaps

- The frontend has a small unit suite (`web/test/*.test.ts`, run by node's built-in test runner via `npm test`, 14 cases over the bulk-import parser and the model search filter). There is no component test runner and no DOM environment, so component behaviour is verified by hand. `npm run build` type-checks with `vue-tsc -b` and lints with Biome; `npm test` is a separate CI step.
- There is no integration test crate. Every test is in-crate `#[cfg(test)]`, so a cross-crate flow is exercised only where a `router-server` route test assembles the full router.
- The DB round trip and the differential harness are not automated. The DB tests use in-memory SQLite, so opening a real shared file from two processes stays a manual check.
