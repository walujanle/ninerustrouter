# Runtime Behaviour: The Process Is a Daemon, Not Just a Router

The server does more than answer `/v1/*`: it runs three autonomous schedulers, holds several
process-global state maps, applies an outbound proxy setting at boot and on every settings write,
and owns a second listener for the Codex OAuth callback. Reading the route table alone misses all of
it. This document is the map of that behaviour, and the invariants a change has to preserve.

## Startup sequence

`serve_once` in `crates/ninerustrouter/src/main.rs` runs the steps in this order:

| Order | Step | Where |
|---|---|---|
| 1 | Install the console-log capture layer | `main.rs::init_tracing`, `services/console_log.rs` |
| 2 | Open the shared DB and migrate | `Db::open`, `router_db::migrations::migrate_on_boot` |
| 3 | Start the WAL checkpoint task | `Db::spawn_checkpoint_task` |
| 3b | Start the idle memory-reclaim thread | `router_server::reclaim::spawn` |
| 4 | Apply the stored outbound proxy | `executors::http::apply_outbound_proxy_settings` |
| 5 | Sweep null optional fields off old connection rows | `repos::connections::cleanup_provider_connections` |
| 6 | Build `AppState` | `AppState::new` |
| 7 | Start the schedulers | `services::schedulers::start_all` |
| 8 | Bind the listener | `main.rs`, `--port` / `--host` |

The DB has to be ready and migrated before any scheduler starts, because the schedulers read
connections the moment they come up. Auto-ping running before the adapter is a real failure mode,
not a theoretical one.

`init_tracing` installs `ConsoleLogLayer` (`services/console_log.rs`), a `tracing` layer that pushes
each event's `message` field into the in-memory ring the SSE route streams. It runs under its own
`EnvFilter` pinned to the app targets at info+ (`ninerustrouter`, `router_server`, `router_db`,
`router_sse`) rather than derived from `RUST_LOG`, so the page is never blank when `RUST_LOG` is
unset. The ticker that drains the pending batch (100 ms) is a plain detached OS thread, not
`tokio::spawn`: the layer is installed before the runtime exists, so the first log line would
otherwise panic on a spawn with no reactor.

The database is shared with 9router and the schema is byte-identical. Tables, column order, types,
defaults, indexes and the PRAGMA set match exactly, so a database file can move between the two
installs. Treat that as an external contract: a row ninerustrouter does not recognise is left
untouched, never deleted. `docs/DB-PARITY.md` is the detail.

## Memory and reclaim

The process holds two heaps: mimalloc for Rust, the C allocator for the bundled SQLite. Neither
returns freed pages to the OS on its own schedule that matches a burst's end, so the resident set
stays at its peak long after the traffic stops. Two mechanisms bound it.

**While working, caps.** The SQLite pool is four connections (`DEFAULT_POOL_SIZE`), each with a
2 MB page cache (`MEMORY_PRAGMA_SQL`, connection-local, never in the shared file's header). The
reqwest client cache holds at most 32 clients, each with a 32-connection idle pool. The tokio
runtime runs 4 to 8 worker threads and 32 blocking threads.

**When idle, reclaim.** `router_server::reclaim` counts the response streams in flight. A request
body increments the counter at construction and decrements it on drop, whether it completes or the
client disconnects. A thread ticks every 5 seconds (`TICK`) and, after 6 idle ticks — 30 seconds with
the counter at zero — trims every heap the burst touched. The C heap gets a per-OS call:
`malloc_trim(0)` on glibc, `mallopt(M_PURGE)` on Android, `malloc_zone_pressure_relief` on macOS, a
working-set trim on Windows. The Android call resolves `mallopt` through `dlsym` because it is API 26
and the build targets API 21; on an older platform the trim is a no-op.

The Rust heap needs the same treatment, and the naive version does not work. mimalloc 3 keeps a
separate heap per thread, and `mi_collect` only collects the *calling* thread's. The memory a burst
leaves behind is almost entirely in the tokio workers' heaps, so a collect from the idle thread alone
freed 576 kB of a 94 MB heap. The trim therefore also spawns one task per worker (bounded by
`available_parallelism`, clamped to 16); a task spawned on the runtime is polled by a worker, so its
`mi_collect` runs in that worker's thread. `spawn_blocking` would run on a blocking-pool thread, the
wrong heap, and `block_in_place` panics on a current-thread runtime. Any heap not visited in one
window is caught in the next, so the footprint converges without a thread per worker.

Only request streams are counted. The dashboard's console-log and stats feeds are long-lived, so
counting them would hold the process busy for as long as a tab is open and the trim would never run.
The trim never overlaps a live response, so it cannot page out bytes a client is still reading.

**Allocator tuning at boot.** `tune_allocator` (`reclaim.rs`) runs before anything allocates, the
only point mimalloc honours the arena size. It sets `mi_option_purge_delay` to 1 ms,
`mi_option_arena_purge_mult` to 1 and `mi_option_page_full_retain` to 0, and logs the values read
back so a mismatch is visible. The defaults hold a burst's pages for ~100 s (the arena delay is the
base delay times a multiplier of 10); 1 ms with a multiplier of 1 makes a purge effectively
immediate, which is what lets idle RSS settle at its resting figure instead of the peak. The delay is
not zero: `mi_arenas_try_purge` returns early when it is `<= 0`, so zero would disable arena purging
entirely. The option ids are spelled by position because the sys crate exports constants only up to
the legacy names.

**`TCP_NODELAY` is set on every accepted socket** (`app.rs`). Without it a keep-alive round trip
pays the peer's delayed-ACK stall, around 40 ms on the common stacks, which dominates the cost of a
small response.

Set `NINERUSTROUTER_MEM_REPORT=1` to log a resident-memory sample every 10 seconds to the console log
page; it is diagnostic only and off by default.

## Schedulers

Three timer loops fire outbound requests with no inbound request to trigger them. All three are
started from `services::schedulers::start_all`.

**1. Proactive OAuth token refresh.** 30-minute lead, 5-minute interval, sequential inter-account
delays (`1.5s + 200ms` jitter) so a batch of refreshes does not look like credential stuffing.
Without it, tokens expire mid-session. See `OAUTH-AND-CREDENTIALS.md`.

**2. Quota auto-ping.** `services/schedulers.rs` drives the Codex executor to keep quota windows
warm. It *warms* windows; it does not disable anything. The auto quota tracker
(`crates/router-db/src/repos/quota_tracker.rs`, `quotaAutoTrackerEnabled`, off by default) rides
the same usage read and is the piece that acts on an exhausted window.

**3. Model catalog sync.** `services/model_catalog_sync.rs` downloads
`https://models.dev/api.json` and writes `DATA_DIR/model-catalog.json`. `CATALOG_VERSION` is 2.

## Ephemeral in-memory state

Deliberately not persisted. Losing a load-bearing row changes behaviour silently, so a change that
reimplements one of them has to keep its semantics.

| State | Where | Load-bearing? |
|---|---|---|
| Login lockout buckets | `auth/login_limiter.rs` (`Limiter.attempts`) | No — reset on restart is correct |
| Combo round-robin rotation index | `services/combo.rs` (process-global rotation state) | **Yes**: losing it changes which account serves a request |
| Token-refresh dedup futures | `services/single_flight.rs` | No |
| CLI-token cache | `state.rs` (`AppState::cli_token`, a `OnceLock`) | No |
| Update-check cache | `services/update_check.rs` | No |
| Config snapshot | `router-db/src/config_cache.rs` (one `Arc<ConfigSnapshot>`, dropped by the SQLite update hook and by a 2 s TTL) | No — rebuilt from SQLite on the next read |

Only the load-bearing row needs its semantics carried forward. The rest are explicitly
"reset on restart, and that is correct"; the comments at each site say so, so a future reader does
not "fix" it.

## Configuration snapshot cache

The chat hot path needs `settings`, `combos`, `modelAliases` and `providerNodes` on every request —
`settings` for `requireApiKey`, the combo strategy and the token-saver flags; `combos` for model
resolution; aliases and nodes for `getModelInfo`. Reading each one is a `SELECT` plus a full JSON
parse, and the data only changes when the dashboard writes it, so a request used to pay that nine to
twelve times over.

`router_db::config_cache` holds one `Arc<ConfigSnapshot>` per process, and a request clones the `Arc`
instead (`handlers/chat.rs`, `routes/v1.rs`). Two things drop it:

- the **SQLite update hook** (`install_config_hook` in `driver.rs`), on a write to `settings`,
  `combos`, `providerNodes` or `kv`. The hook sees no connection, so it clears unconditionally; one
  process serves one database, and with several the only cost is a reload.
- a **2 s TTL**, for the other direction: the application sharing the SQLite file — 9router, or a
  second ninerustrouter — can change a row without this process knowing. The TTL goes in the
  `db/config_cache.rs` header, not at a call site.

`usageHistory` is written on every completed request and is not one of the cached tables, so the
per-request write path does not thrash the cache. A miss only costs a reload.

## Outbound proxy

`executors/http.rs` holds the process-wide outbound proxy behind an `RwLock` (`OUTBOUND`). A Rust
HTTP client is per-call, so the setting is read and a client built for each request rather than
patching a global fetch; an externally-set `HTTP_PROXY` is still consulted through `env_proxy_url`
when the setting is off. `apply_outbound_proxy_settings` runs at boot (`main.rs`) and on every
settings write (`routes/settings.rs`), which is what makes "proxy settings take effect without
restart" true. A read failure must not stop the server: the proxy stays off and the next settings
write applies it.

This is a cross-cutting concern touching every upstream call. Any new outbound client must resolve
its proxy through the same path.

TLS trust is the other cross-cutting concern, and `http::tls_builder()` is its choke point. On
desktop it is `reqwest::Client::builder()` verbatim: the OS trust store, no behavioural change. On
`target_os = "android"` it calls `ClientBuilder::tls_certs_only` instead, because reqwest's
`rustls` feature wires up `rustls-platform-verifier`, whose only Android backend is JNI and panics
at the first handshake under Termux (`android.rs:90`), which has no JVM or `Context`. The store is
Termux's own CA bundle (`rustls-native-certs` → `openssl-probe`, which hardcodes the Termux path)
unioned with the bundled Mozilla roots from `webpki-root-certs`. The union is deliberate:
`tls_certs_only` with no roots trusts nothing, so a Termux install without `ca-certificates` would
fail every HTTPS call with `UnknownIssuer` instead of panicking. The bundled set keeps the store
non-empty. The consequence is that on Android the trust store is the bundled set plus Termux's,
not the Android system store — a routing gateway only needs public CAs, and the desktop path is
untouched. A new outbound client goes through `tls_builder()` for the same reason it goes through
the proxy path.

**The MITM DNS bypass matches on a dot boundary, never by substring.** `should_bypass_mitm_dns`
accepts a host that is the listed name exactly or ends with `.{name}`. A substring test would let
`api2.cursor.sh.attacker.example` inherit the bypass — and the bypass pins the address to a
DoH-resolved IP, so the wrong match means a request to attacker-controlled DNS is routed somewhere
it should not reach. `no_proxy` matching in `should_bypass_by_no_proxy` uses the same exact-or-dot-
boundary rule, with `*` and a leading `.` (which also matches the bare suffix) as its other forms.

Every resolved outbound send logs one line at `target: "router_sse::proxy"`: `[ProxyFetch] proxy ->
<host> via <proxy> (<source>)`, or `direct`, or `relay` for the Vercel relay. The source tag is
`connection` / `outbound` / `env`, so a proxy set on the connection is distinguishable from one
inherited from the global setting or the environment. Only scheme, host and port are logged
(`log_host`): a provider URL can carry a key in its query string and a proxy URL can carry
`user:pass@`, and neither belongs in a log line. Two cases that would otherwise read as a broken
proxy say so explicitly — a host excluded by `no_proxy` logs `connection proxy bypassed by no_proxy`
before going direct, and a proxy that fails to build logs `proxy failed, falling back to direct`, or
under `strictProxy` logs `strictProxy, refusing direct fallback` and returns `SendError::StrictProxy`.

## OAuth loopback listeners

Only the Codex flow spins a throwaway HTTP server on localhost. It uses a fixed port (`1455`) that
its OAuth client allows and nothing else, so the server owns it on a second axum listener in
`services/codex_proxy.rs`. Every other flow has no loopback listener: the redirect lands on the
dashboard's callback page, which relays the code back over `postMessage` / `BroadcastChannel` /
`localStorage` (see `docs/FRONTEND.md`). The server does not shell out to a browser — it hands the
URL to the frontend, which opens it.

Fixed ports can collide with other tools; handle bind failure with a clear error rather than a
panic. `services/codex_proxy.rs` returns the failure through the session status instead of
aborting.

## Updater and the version surface

A static binary cannot replace itself in place, so the update action is a check plus a shutdown, not
a self-replace. `POST /api/version/update` stays on `ALWAYS_PROTECTED` and answers 403: the
dashboard's Update button has something to call and gets a clear answer instead of a 404, and the
route spawns nothing, so it is not a kill primitive. `POST /api/version/shutdown` remains, also
`ALWAYS_PROTECTED`, and is what the manual-update flow calls.

### Update check (`services/update_check.rs`)

The check polls the GitHub Releases of `walujanle/ninerustrouter` (`GET /releases/latest`) and
compares the newest release tag to `APP_VERSION`. That is the only signal: the assets are archives, so the
running executable is never byte-comparable to a download and there is no second one to compute.

`GET /api/version` reads the in-memory cache and never blocks on the network; it also kicks a
background refresh when the cache is older than six hours. The scheduler (`configure`, called from
`start_all`) runs one check 60 s after boot and then daily, gated by the `autoUpdateCheck` setting
(default on), start/stop via an `AtomicBool` exactly like `configure_quota_auto_ping`. Failures keep
the last cache and only log, so a rate limit or an offline machine never clears a known-good status.

The response carries `currentVersion`, `latestVersion`, `hasUpdate`, `updateAvailable`, `releaseUrl`
and `checkedAt`. `updateAvailable` and `hasUpdate` are the same flag, and it is what the sidebar
banner keys off.

### The update prompt

There is one install channel, so there is no method to detect: every install is a direct binary
replaced by hand from the release page. `releaseUrl` falls back to the constant `releases/latest`
link when no poll has completed, so the prompt always has somewhere to point. The sidebar offers no
stop-and-replace flow — a static binary cannot replace itself — and the modal's only action is to
open that page. `ninerustrouter update-check` prints the same link offline.

## Launcher behaviour

`ninerustrouter serve` does three things beyond binding the listener, all in
`crates/ninerustrouter/src/launcher.rs`:

1. Kill whatever holds the port before start (`kill_port_holder`, `netstat`/`taskkill` on Windows,
   `lsof`/`ss`/`kill` elsewhere). If neither tool is present the kill is skipped with a warning.
   On Windows the `netstat` parse takes only rows in the `LISTENING` state (`pids_from_netstat`):
   `netstat -ano` also prints a `TIME_WAIT` or `CLOSED` row per recent client, and those carry the
   *client's* pid, so matching the local address alone would kill an unrelated process.
2. Restart the server on crash, up to `RESTART_ATTEMPTS` times.
3. Poll readiness before opening the browser.

`ninerustrouter start` on a TTY is the one mode that does not go headless: the server runs on a
background thread and the main thread shows the interface menu (Web UI, Terminal UI, exit). The
terminal UI is a client under `crates/ninerustrouter/src/cli/` that talks to the running server over
`/api/*` with the in-process CLI token, so it never logs in; a non-TTY `start` runs headless, as
before. Logs never reach the terminal: the binary installs only the console-log capture layer, so
output lands in the in-memory buffer the dashboard's Console Log page streams and cannot corrupt the
TUI's redraw.

The SQLite driver is compiled in, so there is no native runtime to self-heal at boot.

### Environment variables

The variables ninerustrouter defines are listed with their defaults in
`.env.example` at the repo root. ninerustrouter reads them from the process
environment and does not load `.env` itself, so export them or set them in your
service manager. The secrets (`JWT_SECRET`, `API_KEY_SECRET`, `MACHINE_ID_SALT`)
fall back to generated values; see `docs/DB-PARITY.md` for exactly what each one
changes.

Three more are read but deliberately absent from `.env.example`, because they
are not settings a user is expected to change: `DISABLE_BACKGROUND_TOKEN_REFRESH`
and `BG_REFRESH_DELAY_MS` (`services/schedulers.rs`) exist to make the refresh
loop testable, and the standard `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` /
`NO_PROXY` family (and their lowercase spellings) is honoured by the outbound
client in `executors/http.rs`.

### Port and host overrides

`ninerustrouter serve|start|stop` take `-p/--port` and `-H/--host`. The flag wins over the `PORT` and
`HOSTNAME` environment variables, which win over the `20129` / `0.0.0.0` defaults.

The flag has to reach more than the bind. Two routes build a loopback URL back into this same
server — `POST /api/models/test` (`routes/models_test.rs`) and `POST /api/providers/{id}/test-models`
(`routes/providers.rs`) — and they read the port from the environment via `state::resolve_port()`.
`apply_overrides` in the binary pushes the flag into `PORT`/`HOSTNAME` once at startup, before any
thread exists, so those self-referential URLs follow the flag too. A port the flag never reaches
would leave them pointing at 20129 while the listener sits elsewhere, and the probe would fail with
connection-refused instead of a real result.

`DEFAULT_MITM_ROUTER_BASE` in `router-db` stays `http://localhost:20128`. It is a stored-settings
parity value, not a live bind address, so it is not moved to the 20129 default.

## Proxy-pool remote deploys

Four routes in `routes/proxy_pools.rs` generate and deploy relay workers to Cloudflare, Deno Deploy,
Netlify and Vercel, then poll deployment status:
`POST /api/proxy-pools/{cloudflare,deno,netlify,vercel}-deploy`. Each embeds the relay source and
calls the provider API with the user's token. The Cloudflare entry is a relay worker deploy. These
are outbound infrastructure provisioning, not a dashboard-only feature, so a change here has network
side effects outside this process.

Netlify is the odd one: its API is a digest deploy, not a build deploy. `services/netlify_relay.rs`
creates the site, announces SHA1 (file) and SHA256 (function) digests, uploads only the ones Netlify
lists as required, and polls until `state == "ready"`. The function is a CJS `exports.handler`
returning a string body, because ESM syntax and stream returns both answer 502 on that runtime. It
buffers the upstream response, so a stream arrives in one piece and the 60 s synchronous limit
applies on both sides.

The relay workers only move bytes, so the Cloudflare worker asks the origin for
`accept-encoding: identity` and deletes `content-encoding`, `content-length`, `transfer-encoding` and
`connection` before rebuilding the response. `fetch` decodes the body while the copied framing
headers still describe the encoded one, and a response the edge cannot frame is what it answers with
a `520`. A `520`–`527` status on a request that went through the relay is an edge error, not a
provider one: the chat path treats it as transport, so the credential is neither cooled nor rotated
out for it. Direct provider 5xx handling is unchanged.

## Auto quota tracker

`crates/router-db/src/repos/quota_tracker.rs`, gated by `settings.quotaAutoTrackerEnabled` (default
**false**):

- A connection with any exhausted quota window (`is_quota_exhausted`: `remaining <= 0`, else `used
  >= total` with a positive total; `unlimited` never exhausts) is set `isActive: false` and marked
  `quotaAutoDisabled: true` with `quotaAutoDisabledAt`.
- When usage is available again, only a connection carrying that marker is re-enabled and the
  marker cleared. A user-disabled connection has no marker, so the tracker never re-enables it.
- A connection with no quota reading is never disabled — "no data" is not "exhausted".

The tracker runs on the same usage read as the quota auto-ping scheduler, so it only acts while that
loop is running.

`settings.quotaAutoTrackerEnabled` is re-read on every settings write that touches `codexAutoPing`
or `quotaAutoTrackerEnabled` itself, and the auto-ping loop is started or stopped to match. The
toggle takes effect without a restart, the same way the outbound proxy setting does.

## Anti-spoofing foundation

The local-only and CLI-token model rests on the guard in `auth/guard.rs`, fed by
`middleware.rs::facts_from`, which builds `RequestFacts` from `ConnectInfo<SocketAddr>` and the
request headers. Client-supplied `x-forwarded-for` and `x-real-ip` are believed only when the TCP
peer is loopback, so a remote client cannot claim to be local. The `x-9r-cli-token` header is
compared in constant time against a machine id derived in-process (`AppState::cli_token`).

axum reads the socket directly, so no peer token is minted into the frontend — but the *trust
decision* is the invariant: any change to which peers may forward, or to how localness is decided,
changes the security posture of every route at once.
