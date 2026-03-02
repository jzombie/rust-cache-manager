# cache-manager

[![made-with-rust][rust-logo]][rust-src-page] [![crates.io][crates-badge]][crates-page] [![MIT licensed][mit-license-badge]][mit-license-page] [![Apache 2.0 licensed][apache-2.0-license-badge]][apache-2.0-license-page] [![Coverage][coveralls-badge]][coveralls-page]

Directory-based cache and artifact path management with crate-root discovery, grouped cache paths, and optional eviction on directory initialization.

**This crate is intentionally tool-agnostic** — it only manages cache/artifact directory layout and paths and does not assume or depend on any specific consumer tooling. Any tool or library may use it to discover, create, and evict files in project-scoped cache directories.

If your utility reads or writes files at all, it can use `cache-manager` to compute and manage cache paths and apply eviction rules — the crate makes no assumptions about how callers read or write those files.

**It has zero runtime dependencies (standard library only for library consumers).**

It is suitable for:

- Artifact storage (build outputs, generated files, intermediate data, etc.).
- Monorepos or multi-crate workspaces that need centralized cache/artifact management via a shared root (for example with `CacheRoot::from_root(...)`).

> Tested on macOS, Linux, and Windows.

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

## Usage

Basic examples showing common operations:

The primary root type is `CacheRoot`, which represents a filesystem root
under which cache groups (`CacheGroup`) live.

A `CacheGroup` represents a subdirectory under a `CacheRoot` and manages
cache entries stored in that directory.

Discover a cache path for the current crate/workspace and resolve an entry path.

Note: `discover_cache_path` only computes a filesystem path — it does not
create directories or files. Behavior:

- Searches upward from the current working directory for a `Cargo.toml` and
	uses that crate root when found; otherwise it falls back to the current
	working directory.
- The discovered root is canonicalized when possible to avoid surprising
	differences between logically-equal paths.
- If the `relative_path` argument is absolute, it is returned unchanged.

```rust
use cache_manager::CacheRoot;

// Compute a path like <crate-root>/.cache/tool/data.bin without creating it.
let cache_path = CacheRoot::discover_cache_path(".cache", "tool/data.bin");
println!("cache path: {}", cache_path.display());

// If you already have an absolute entry path, it's returned unchanged:
let absolute = std::path::PathBuf::from("/tmp/custom/cache.json");
let kept = CacheRoot::discover_cache_path(".cache", &absolute);
assert_eq!(kept, absolute);
```

Create a `CacheRoot` from an explicit path and apply an eviction policy to a group:

```rust
use cache_manager::{CacheRoot, EvictPolicy};
use std::time::Duration;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts");

let policy = EvictPolicy {
	max_files: Some(100),
	max_age: Some(Duration::from_secs(60 * 60 * 24 * 30)), // 30 days
	..Default::default()
};

group.ensure_dir_with_policy(Some(&policy)).expect("ensure and evict");
```

Preview which files would be removed without applying deletions:

```rust
use cache_manager::{CacheRoot, EvictPolicy};

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts");
let policy = EvictPolicy {
	max_files: Some(10),
	..Default::default()
};

let report = group.eviction_report(&policy).expect("eviction report");
for p in report.marked_for_eviction {
	println!("would remove: {}", p.display());
}
```

Create or update a cache entry (ensures parent directories exist):

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts/json");

let entry = group.touch("v1/index.bin").expect("touch entry");
println!("touched: {}", entry.display());
```

Per-subdirectory policies

Different subdirectories under the same `CacheRoot` can use independent policies; call `ensure_dir_with_policy` on each `CacheGroup` separately to apply per-group rules.

Get the root path

To obtain the underlying filesystem path for a `CacheRoot`, use `path()`:

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_root("/tmp/project");
let root_path = root.path();
println!("root path: {}", root_path.display());
```

Also obtain a `CacheGroup` path and resolve an entry path under that group:

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts");

let group_path = group.path();
println!("group path: {}", group_path.display());

let entry_path = group.entry_path("v1/index.bin");
println!("entry path: {}", entry_path.display());
```


## License

`cache-manager` is primarily distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-APACHE](./LICENSE-APACHE) and [LICENSE-MIT](./LICENSE-MIT) for details.

[rust-src-page]: https://www.rust-lang.org/
[rust-logo]: https://img.shields.io/badge/Made%20with-Rust-black

[crates-page]: https://crates.io/crates/cache-manager
[crates-badge]: https://img.shields.io/crates/v/cache-manager.svg

[mit-license-page]: ./LICENSE-MIT
[mit-license-badge]: https://img.shields.io/badge/license-MIT-blue.svg

[apache-2.0-license-page]: ./LICENSE-APACHE
[apache-2.0-license-badge]: https://img.shields.io/badge/license-Apache%202.0-blue.svg

[coveralls-page]: https://coveralls.io/github/jzombie/rust-cache-manager?branch=main
[coveralls-badge]: https://img.shields.io/coveralls/github/jzombie/rust-cache-manager
