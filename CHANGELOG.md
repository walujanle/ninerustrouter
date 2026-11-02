# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is semantic.

## [Unreleased]

### Fixed

- `build-android.sh` no longer aborts before building. It exported
  `CC_aarch64-linux-android`, which is not a legal shell identifier, so the
  export failed and `set -e` stopped the script at that line. The per-target
  compiler variables are now spelled with underscores, the form `cc-rs` also
  accepts.

## [0.1.2] - 2026-11-02

### Changed

- Linux, macOS and Android releases are published as `.tar.gz` instead of
  `.zip`. The tarball keeps the executable bit, so the unpacked binary runs
  without a `chmod +x` first. Windows still ships `.zip`.

## [0.1.1] - 2026-11-02

### Added

- Initial release.

### Fixed

- The declared toolchain floor is Rust 1.89. `rusqlite` 0.41 requires it, so
  the previous 1.88 claim no longer built.
