# Changelog

All notable changes to the `phi-ns-desktop` crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-11-27

### Changed
- **Dual-licensed as MIT OR Apache-2.0.** The crate's license field now carries
  both, matching the rest of the family.
- **CI runs on every release platform.** Linux, macOS and Windows all build and
  test; a platform-primitive gate rejects hand-rolled `HOME` lookups.

## [0.1.0] - 2026-10-31

### Added
- **First release.** The `desk.*` capability pack: screen and accessibility observation, pointer
and keyboard input, clipboard and window steering, over the `phi-ns`
contract.
