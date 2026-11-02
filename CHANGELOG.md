# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is semantic.

## [Unreleased]

## [0.1.1] - 2026-11-02

### Added

- Initial release.

### Fixed

- The declared toolchain floor is Rust 1.89. `rusqlite` 0.41 requires it, so
  the previous 1.88 claim no longer built.
