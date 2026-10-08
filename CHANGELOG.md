# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/) and this project adheres to
 (or is loosely based on) Semantic Versioning.

## [0.6.0] - 2026-10-07

### Added

- `ensure_writable_tree` + `WritableReport`: normalize-on-ingest permission
  pass over an extracted cache tree. Sets owner-write on Unix (owner rwx for
  directories so traversal never EACCES, owner-write only for files —
  group/other bits untouched) and clears the readonly flag elsewhere.
  Symlinks are never followed or modified, traversal stays under the root
  (symlinked roots fail closed), order is deterministic, errors abort naming
  the path. Fixes the observed failure where `0444` release-tarball members
  broke post-extract steps (macOS `xattr -dr` quarantine strips, Windows
  tree deletion on rotation). Covered by `tests/normalize_permissions.rs`:
  readonly fixtures, counts, idempotency, mode preservation, symlink
  non-mutation, file roots, search-less directories, and a macOS-only
  incident reproduction (0444 file rejects the strip before, accepts after).
- `CacheGroup::ensure_dir` / `ensure_dir_with_policy` now normalize on the
  way in: group dirs are always left owner-writable. Callers extracting
  archives re-ensure afterwards — `tar` writes explicit mode bits per member
  during unpack, re-applying read-only modes creation just cleared.

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
