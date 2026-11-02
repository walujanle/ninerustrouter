# Maintenance

This project is feature-frozen. The only work it expects is a dependency bump,
a security fix, or a data update (a provider entry in `registry.json`). This page
is the whole routine for that, and the list of things that break silently if
nobody touches them.

Everything here assumes you have read `AGENTS.md`. The two constraints that
shape the code — port 20129 and the byte-identical shared SQLite schema — are
there, not here.

## The routine

There is no update bot: dependencies move when you move them. A few times a
year, or when `cargo audit` / `npm audit` reports something:

```bash
cargo update                      # every crate to the newest semver-compatible version
cargo update -p <crate>           # one crate
cargo update -p <crate> --precise <version>   # pin one version exactly
cd web && npm update              # within the ranges package.json already allows
cd web && npm outdated            # what is held back, and why
```

Then run the gate (`docs/TESTING.md`) and commit. The two audits fail the build
on a finding, so a green CI run means no known advisory is in the tree.

A dependency that is held back by a range in `Cargo.toml` or `package.json`
needs the manifest edited, not `update`. Widen the range deliberately, as its
own commit, so a bisect can find it.

**A major bump is not a routine bump.** Read its changelog first. Two are
pinned on purpose and move only together with something else: TypeScript at 6.x
(`vue-tsc` needs its `./lib/tsc` export) and the compiler in
`rust-toolchain.toml`.

## Cutting a release

The dependency work above lands on `main` with no release. Tag one when you want
to ship. `docs/RELEASING.md` has the steps; the short version is: write
`PROJECT_VERSION`, run `node scripts/set-version.mjs`, commit, tag `vX.Y.Z`,
push the tag. The release workflow runs the gate again before it builds, so a
green tag cannot publish a binary that fails a check.

A release with no changelog entry is normal here. Only a security fix, a
breaking change, or an added or removed capability belongs in `CHANGELOG.md`.

## What breaks if nobody looks

The code does not rot on its own, but the world around it does. These are the
things that fail without a code change, roughly worst first.

### 1. A provider changes its wire format

The provider list, models and pricing are committed JSON
(`crates/router-sse/src/providers/registry.json`, `catalog.json`), not code, so
adding or correcting a provider is a data edit. What is *not* data is a change
to how a provider's endpoint behaves: a new required header, a moved auth URL, a
renamed field. That needs a code change in `crates/router-sse/src/executors/`.

Symptom: requests to one provider return 400 or 401 while every other provider
works. Read `docs/CHAT-PIPELINE.md`, then the executor for that provider.

### 2. A TLS root expires or a CA rotates

Outbound HTTPS uses the system trust store on desktop and a bundled one on
Android (`crates/router-sse/src/executors/http.rs`). A root expiring is
invisible until every provider fails TLS at once. Symptom: all providers fail
with a certificate error. Fix: bump `rustls-native-certs`, or the bundled roots.

### 3. The crates.io ecosystem moves past a pinned version

`rust-toolchain.toml` pins the compiler (1.97.1) so a new stable cannot break
the build, and `Cargo.lock` is committed so the tree does not float. Both are
deliberate: this repo builds the same way in three years as it does today. The
cost is that a *dependency* only moves when someone bumps it, so run the
routine at the top of this page a few times a year and read the gate output.

The MSRV floor (`rust-version` in `Cargo.toml`, 1.88) is exercised by its own CI
job. If a dependency bump raises the real MSRV above that floor, that job fails
and tells you: either bump the floor or hold the dependency.

### 4. TypeScript 7, and the `vue-tsc` pin

Two frontend packages are pinned exactly rather than by caret, and they are
pinned for two different reasons.

**TypeScript is held at 6.x.** TypeScript 7 removes the `./lib/tsc` export that
`vue-tsc` reads, so `npm run build` fails with `Package subpath './lib/tsc' is
not defined by "exports"`. Both were tested; there is no way to move TypeScript
alone today.

**`vue-tsc` is held at 3.3.13.** Version 3.3.14 mis-binds a property access
after a bare identifier inside a template interpolation, so
`{{ APP_CONFIG.version }}` type-checks as a property of the string `v` and
`vue-tsc` errors with `Property 'value' does not exist`. Nothing in the source
changed; the same code passes on 3.3.13. Do not "fix" the templates to satisfy
3.3.14 — the templates are correct and the bug is upstream.

When a `vue-tsc` release fixes that, move the pair together and confirm with
`npm run build`. The CI frontend job fails loudly if either moves alone.

### 5. A provider is retired, or a new one is wanted

An edit to `registry.json` plus `catalog.json`, or a deletion. The registry is
the source of truth and the frontend reads it over `GET /api/registry`, so there
is no second copy to update. A provider with no PNG under
`web/public/providers/` renders an initial badge; a missing icon is not a bug.

### 6. The database schema moves upstream

9router owns the schema (`SCHEMA_VERSION` in `crates/router-db/src/schema.rs`)
and this project follows. If upstream adds a table or a column, this build keeps
working — it leaves rows it does not recognise untouched, and the additive sync
only creates what it knows. Nothing here needs to change to keep reading a newer
database. See `docs/DB-PARITY.md` before touching the schema itself.

## Adding a provider by hand

Adding a provider is the one recurring data task. The steps are in
`docs/PLAN.md` under "Adding a provider": add the entry to `registry.json`, and
add a special executor only if the provider does not speak the OpenAI wire
format. Three copies of the provider data have to move together — the entry's
`transport` against the built `providers[<id>]` map, and the entry's `models`
against `providerModels[<alias>]`. Editing one and not the others is the drift
that bites, because the executor reads the built map while the dashboard reads
the entry.

## When in doubt

Read the document that owns the area. The table at the top of `AGENTS.md` maps
each area to its page, and `docs/CODEBASE-MAP.md` maps a file to its purpose.
