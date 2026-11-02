# Releasing NineRustRouter

One tag on the default branch (`main`) builds six platform zips and attaches
them to a GitHub Release with notes pulled from `CHANGELOG.md`. The workflow is
`.github/workflows/release.yml`. A release publishes no package to any registry.

## Version: one source of truth

`PROJECT_VERSION` at the repo root holds `x.y.z` — nothing else, no `v`, no
`-beta`. `scripts/set-version.mjs` validates it against `^\d+\.\d+\.\d+$` and
exits 1 on anything else, so a prerelease version is not expressible and no
workflow that runs `--check` can ever accept a `-rc` tag. `scripts/set-version.mjs`
propagates it into every manifest that carries a version:

```
Cargo.toml                    [workspace.package] version
web/package.json              version  (shown in the UI as APP_CONFIG.version)
```

The running binary reports `APP_VERSION`, which is `env!("CARGO_PKG_VERSION")`,
so syncing `Cargo.toml` is what makes `/api/version` agree with the release.

`set-version.mjs` rewrites the two lock files as well — `web/package-lock.json` and the four
workspace-member entries in `Cargo.lock`. Every build path runs `--locked`, so a lock left at the
old version makes `npm ci` refuse the tree and `cargo build --locked` fail on the stale member.
`PROJECT_VERSION` stays the only place a version is edited.

```bash
node scripts/set-version.mjs          # write PROJECT_VERSION into every manifest
node scripts/set-version.mjs --check  # exit 1 if any manifest disagrees (CI runs this)
```

## Cut a release

1. Roll the changelog: move whatever sits under `## [Unreleased]` to a new
   `## [x.y.z] — YYYY-MM-DD` heading. Only release-visible changes are recorded there
   (security fixes, breaking changes, added or removed capability); everything else is
   documented in `docs/`, so a release with nothing to announce is normal and is a no-op
   here. Leave the `[Unreleased]` heading in place; `scripts/extract-changelog.mjs`
   falls back to it, so an empty section still produces a release body.
2. Write the version:

   ```bash
   echo "x.y.z" > PROJECT_VERSION
   node scripts/set-version.mjs
   ```

3. Commit, then tag and push:

   ```bash
   git add -A && git commit -m "chore(release): x.y.z"
   git tag "vx.y.z"
   git push origin main "vx.y.z"
   ```

The tag push starts the workflow. It refuses to build if the tag (minus `v`)
does not equal `PROJECT_VERSION`, or if any manifest has drifted.

**Creating the Release is re-runnable.** The step skips when a Release for the
tag already exists (`gh release view` first), so a re-run after a failed publish
leg does not abort on `release vX.Y.Z already exists` — which it used to, after
the assets had already been built.

`workflow_dispatch` runs the build matrix without publishing — use it to smoke a
leg before tagging.

Every push to `main` and every pull request runs `.github/workflows/ci.yml`:
`scripts/set-version.mjs --check`, `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace`, the
frontend `npm run build`, `biome ci`, `npm test`, `cargo audit` and `npm audit
--audit-level=high`. The release workflow's `gate` job runs the same Rust and
audit checks on the tag before any artifact is built, so a tag pushed at a
commit that never went through a pull request still cannot publish a binary that
fails a check.

## What the workflow produces

Each leg builds one zip holding the bare executable. The executable inside is
always plain `ninerustrouter` (or `ninerustrouter.exe`) — the version lives in
the zip's name, never in the file you run.

| Asset (zip) | Contains | Runner | Target |
|---|---|---|---|
| `ninerustrouter-linux-amd64-x.y.z.zip` | `ninerustrouter` | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` |
| `ninerustrouter-linux-arm64-x.y.z.zip` | `ninerustrouter` | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` |
| `ninerustrouter-linux-android-aarch64-x.y.z.zip` | `ninerustrouter` | `ubuntu-24.04` (own job) | `aarch64-linux-android` |
| `ninerustrouter-macos-arm64-x.y.z.zip` | `ninerustrouter` | `macos-latest` | `aarch64-apple-darwin` |
| `ninerustrouter-windows-x64-x.y.z.zip` | `ninerustrouter.exe` | `windows-latest` | `x86_64-pc-windows-msvc` |
| `ninerustrouter-windows-arm64-x.y.z.zip` | `ninerustrouter.exe` | `windows-11-arm` | `aarch64-pc-windows-msvc` |

The Linux assets are glibc, not musl. The rustls backend is `aws-lc-rs`, which
compiles C and assembly through cmake; a musl cross-build would need a cross C
toolchain for `aws-lc-sys`, `libsqlite3-sys` and `zstd-sys` at once. Native
runners avoid that entirely. Revisit musl (via `cargo-zigbuild`) only if someone
needs a static binary.

Android is a separate job on `ubuntu-24.04` (x64), not a matrix leg. `cargo ndk`
needs the NDK, and Google publishes it only for an x86_64 Linux host — there is
no `linux-aarch64` NDK archive — while the `ubuntu-24.04-arm` image ships no
Android SDK at all. The x64 image presets `ANDROID_NDK_HOME` to NDK 27.3, so the
cross-compile to `arm64-v8a` runs there.

Each leg downloads the frontend `web/dist` artifact — `router-server` embeds it
with `rust-embed`, so the frontend is built once and shared. The Linux and macOS
legs zip with the `zip` CLI; the Windows legs use `Compress-Archive`, since no
`zip` is shipped. Each zip is uploaded with a `<asset>.zip.sha256` sidecar
carrying its hex SHA-256.

The zip is what gets hashed, not the binary inside it: the update check compares
the release tag against the running version and nothing else, so there is no
executable digest to publish.

## Install channels

One, and it does not go through a package registry:

- **GitHub Release zip.** Download, unpack, put the executable on `PATH`. This is
  the only install path, and it is what the in-app update prompt points at.

There is no container image and no npm package. Releases up to 0.3.x published
`ninerustrouter` and five platform packages to npmjs.com; 0.3.2 is the last one.
Termux takes the `ninerustrouter-linux-android-aarch64-*` zip directly.

Building from source is the only other route: `./build.sh` (Linux/macOS) or
`build.bat` (Windows), each of which writes `ninerustrouter-binary/` and, with
`--release-name`, the same versioned zip CI publishes.

## Removed: npm publishing

Releases up to 0.3.2 published six packages (`ninerustrouter` plus five platform
packages) to npmjs.com through Trusted Publishing (OIDC). That channel is gone:
the `publish-npm` job is out of `release.yml`, the `npm/` tree is deleted, and
nothing in the repo reads or writes the registry.

The old names stay on npmjs.com at their last version. Deprecate them there
rather than deleting them, so an existing `npm i -g ninerustrouter` install fails
with a pointer to the release page instead of a 404:

```bash
npm deprecate ninerustrouter "No longer published. Install from https://github.com/walujanle/ninerustrouter/releases/latest"
for p in linux-x64 linux-arm64 darwin-arm64 windows-x64 windows-arm64; do
  npm deprecate "ninerustrouter-$p" "No longer published. See https://github.com/walujanle/ninerustrouter/releases/latest"
done
```

## Verifying a release

- `node scripts/extract-changelog.mjs x.y.z` prints the notes that will be used.
- The Release should carry six zips, each with a `.zip.sha256` sidecar.
- Unzip the asset for your platform; the archive holds a bare `ninerustrouter`
  (or `ninerustrouter.exe`), and `ninerustrouter --version` prints the released
  version. The binary runs with no Node installed.
- On Termux, the `ninerustrouter-linux-android-aarch64-*` asset runs under Bionic.
- A running older binary reports `updateAvailable: true` on `/api/version` and
  shows the sidebar banner within a couple of seconds of load.
