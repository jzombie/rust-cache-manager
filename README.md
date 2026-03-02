# cache-manager

[![made-with-rust][rust-logo]][rust-src-page]

Directory-based cache and artifact path management with crate-root discovery, grouped cache paths, and optional eviction on directory initialization.

This crate is intentionally tool-agnostic — it only manages cache/artifact directory layout and paths and does not assume or depend on any specific consumer tooling. Any tool or library may use it to discover, create, and evict files in project-scoped cache directories.

**It has zero runtime dependencies (standard library only for library consumers).**

It is suitable for:

- Artifact storage (build outputs, generated files, intermediate data, etc.).
- Monorepos or multi-crate workspaces that need centralized cache/artifact management via a shared root (for example with `CacheRoot::from_root(...)`).

## Eviction Policy

Use `EvictPolicy` with:

- `CacheGroup::ensure_dir_with_policy(...)`
- `CacheRoot::ensure_group_with_policy(...)`
- `CacheGroup::eviction_report(...)` to preview which files would be evicted.

Policy fields:

- `max_age`: remove files older than or equal to the age threshold.
- `max_files`: keep at most N files.
- `max_bytes`: keep total file bytes at or below the threshold.

Eviction order is always:

1. `max_age`
2. `max_files`
3. `max_bytes`

For `max_files` and `max_bytes`, files are evicted oldest-first by modified time (ascending), then by path for deterministic tie-breaking.

`eviction_report(...)` and `ensure_*_with_policy(...)` use the same selection logic.

### How `max_bytes` works

- Scans regular files recursively under the managed directory.
- Sums `metadata.len()` across those files.
- If total exceeds `max_bytes`, removes files oldest-first until total is `<= max_bytes`.
- Directories are not counted as bytes.
- Enforcement happens only during policy-aware `ensure_*_with_policy` calls (not continuously in the background).

## License

`cache-manager` is primarily distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-APACHE](./LICENSE-APACHE) and [LICENSE-MIT](./LICENSE-MIT) for details.

[rust-src-page]: https://www.rust-lang.org/
[rust-logo]: https://img.shields.io/badge/Made%20with-Rust-black
