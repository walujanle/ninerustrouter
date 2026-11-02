# NineRustRouter

A local AI routing gateway, written in Rust, with a Vue 3 dashboard. One
OpenAI-compatible endpoint (`/v1/*`) fans out across the upstream providers you
configure, with format translation, model-combo fallback, multi-account
fallback, OAuth and API-key credential management, token refresh, usage
tracking, and an optional quota tracker.

NineRustRouter is a Rust implementation of the 9router gateway. Its design,
feature set and wire behaviour follow [9router](https://github.com/decolua/9router)
by [decolua](https://github.com/decolua), used as the reference implementation;
that project's MIT notice is retained in `LICENSE`. NineRustRouter listens on
**20129** and uses a byte-identical SQLite schema, so a database can move
between the two installs with no migration step and the two can run side by
side.

## Install

Download the zip for your platform from the
[latest release](https://github.com/walujanle/ninerustrouter/releases/latest),
unpack it, and put the executable on your `PATH`. The zip is named
`ninerustrouter-{os}-{arch}-{version}.zip`; the executable inside is always
plain `ninerustrouter` (or `ninerustrouter.exe`). There is no npm package and no
runtime dependency on Node.

```bash
# linux x86_64; pick the asset matching your OS/arch and set VERSION to it
VERSION=0.1.0
curl -fLO "https://github.com/walujanle/ninerustrouter/releases/download/v$VERSION/ninerustrouter-linux-amd64-$VERSION.zip"
unzip "ninerustrouter-linux-amd64-$VERSION.zip"        # -> ./ninerustrouter
install -m 755 ninerustrouter ~/.local/bin/ninerustrouter
```

Each zip ships a `.zip.sha256` sidecar. On Termux, take the
`ninerustrouter-linux-android-aarch64-*` zip — the glibc Linux build cannot load
under Bionic.

Or build from source:

```bash
cargo build --release      # embeds web/dist into the binary
./build.sh                 # or build.bat on Windows: frontend first, then cargo
```

Both scripts write `ninerustrouter-binary/`. Pass `--release-name` to `build.sh`
to also emit the same versioned zip CI publishes.

## Run

```bash
ninerustrouter serve                    # run in the foreground
ninerustrouter start                    # kill whatever holds the port, run, restart on crash
ninerustrouter stop                     # stop the process holding the port
ninerustrouter serve --port 8080        # override the port (also -p)
ninerustrouter serve -H 127.0.0.1       # bind one interface instead of 0.0.0.0
```

Then open `http://localhost:20129/dashboard`, add a provider, and point your
client at `http://localhost:20129/v1`.

## What it does

One endpoint, many providers. A request in OpenAI, Anthropic, Gemini, or
Responses format is translated to whatever the selected provider speaks, and the
reply is translated back.

When a model fails, the combo moves to the next model; when an account fails,
the request moves to the next connection for that provider. Providers that need
OAuth get an OAuth flow, the rest take API keys, and tokens refresh proactively
and on a 401/403. A model that keeps erroring is cooled down.

Usage is accounted per provider, model, and account — tokens and cost — into the
shared SQLite tables. An optional quota tracker (off by default) disables a
connection whose quota is exhausted and re-enables it once usage is available
again.

Everything is local: no cloud sync, no telemetry. The binary has no runtime
dependency on Node.

## Layout

```
crates/
  router-db/       SQLite: schema, migrations, repos. The shared-schema boundary.
  router-sse/      Registry data, translators, executors, chat pipeline. No HTTP.
  router-server/   axum: /v1*, dashboard /api/*, auth, OAuth, static assets.
  ninerustrouter/      bin: clap (serve | start | stop | update-check)
web/               Vue 3 + Vite + Tailwind + Biome dashboard
scripts/           set-version.mjs, extract-changelog.mjs
docs/              design docs; start at docs/README.md
```

`PROJECT_VERSION` at the root is the single version source; `node
scripts/set-version.mjs` propagates it into every manifest. Cutting a release is
`docs/RELEASING.md`.

## Build and verify

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo audit

cd web && npm run build       # vue-tsc -b && biome check && vite build
cd web && ./node_modules/.bin/biome ci
cd web && npm audit
```

Toolchain floor is Rust 1.88 (`edition = "2024"`). Two frontend packages are
held deliberately: TypeScript at 6.x, and `vue-tsc` at 3.3.13. TypeScript 7
removes the `./lib/tsc` export `vue-tsc` 3.3 needs, and `vue-tsc` 3.3.14
mis-binds `a.b` after an identifier inside a template interpolation
(`{{ APP_CONFIG.version }}` fails to type-check), so the two move together with
a working pair, not one at a time. See `docs/MAINTENANCE.md`.

## Contributing

`AGENTS.md` has the rules that are easy to get wrong (JSON byte parity, date
formats, the registry, the shared database). Read it before changing
anything structural; the per-area docs it points to are the next step.

The project is feature-frozen: its ongoing work is dependency updates, security
fixes, and provider data. `docs/MAINTENANCE.md` is the routine for that, and
`docs/README.md` is the map of the rest.

## Security

`SECURITY.md` covers the trust model, the accepted risks, and what the gateway
stores and logs. Report anything exploitable privately, not in a public issue.

## License

MIT.
