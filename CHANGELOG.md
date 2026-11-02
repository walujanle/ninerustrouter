# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is semantic.

## [Unreleased]

## [0.1.4] - 2026-11-03

### Fixed

- The native Termux Android build compiles its bundled C. It used plain `clang`,
  so the `cc` crate passed `--target=aarch64-linux-android` with no API level,
  and Termux's `ndk-sysroot` `<sys/cdefs.h>` rejects an unversioned triple
  outright (`#error Unversioned target triples are not supported!`). Every C
  probe and the `jitterentropy` sources inside `aws-lc-sys` failed on it. The
  compiler is now `aarch64-linux-android-clang` from the same `clang` package, a
  wrapper that injects the versioned triple.

## [0.1.3] - 2026-11-03

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
