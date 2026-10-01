# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/) and this project adheres to
 (or is loosely based on) Semantic Versioning.

## [0.5.0] - 2026-10-01

### Added

- `CacheResolver` + `CacheSource`: combined cache-root resolution with fixed
  precedence (explicit path → env var → Cargo workspace discovery → OS user
  cache) and no silent `<cwd>/.cache` scattering. Unresolvable configurations
  return `Err(NotFound)` naming the env var / OS identity that would fix it;
  the `<cwd>/.cache` last resort is an explicit `allow_cwd_fallback` opt-in.
  The winning source is always returned so binaries can announce cache
  placement. Includes resolver unit tests (precedence matrix, `NotFound`
  defaults, panic-safe env guards) and a README section documenting the
  contract and the announce-the-winner convention.

### Fixed

- README doctests for `from_project_dirs` failed under default features
  (`directories` unavailable, method compiled out). Examples are now
  feature-gated `run()` functions: fully compiled and executed wherever the
  `os-cache-dir` feature is enabled, never `ignore`d, never stale.
