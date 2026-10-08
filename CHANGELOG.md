# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/) and this project adheres to
 (or is loosely based on) Semantic Versioning.

## [0.6.0] - 2026-10-07

### Added

- `ensure_writable_tree` + `WritableReport`: call once after extracting an
  archive into its final cache location. Makes the tree owner-writable so
  post-extract steps don't fail on read-only tarball members (e.g. `0444`
  files breaking macOS `xattr -dr` strips, Windows deletion on rotation).
  Unix: adds owner-write only (owner rwx for directories so traversal never
  EACCES, owner-write for files; group/other bits untouched). Non-Unix:
  clears the readonly flag. Never follows or modifies symlinks; symlinked
  roots fail closed; deterministic order; first I/O error aborts naming
  the path. Returns counts of fixed files/dirs; second run reports zeros.
- `CacheGroup::ensure_dir` / `ensure_dir_with_policy` now leave group dirs
  owner-writable. Since `tar` re-applies per-member modes during unpack,
  call `ensure_writable_tree` again after unpacking into the group dir.

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
