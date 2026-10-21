# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-21

### Added

- Prebuilt ARM64 Linux archives in GitHub releases.
- Interactive, source-aware document search with match navigation, status, and
  configurable smart, sensitive, or insensitive case handling.

### Changed

- Nix exposes source builds as `yomu` and checksummed GitHub release binaries
  as `yomu-bin`.
- Built-in graphical Themes align every heading level, emphasize H1 through H3
  with rasterized type, and distinguish H4 through H6 without variable-width
  Markdown markers.

## [0.1.0] - 2026-10-18

### Added

- Yomu, a terminal Markdown reader for local files and standard input.
- Source-aware navigation, responsive layout, syntax-highlighted code, tables,
  local images, and rendered headings and math with readable text fallbacks.
- Automatic Text, Kitty Graphics, and experimental sixel presentation with
  explicit backend selection and actionable fallback diagnostics.
- Built-in and user-defined Themes for terminal text and rasterized content.

[Unreleased]: https://github.com/choplin/yomu/commits/main
