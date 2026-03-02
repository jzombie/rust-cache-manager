# cache-manager

[![made-with-rust][rust-logo]][rust-src-page] [![crates.io][crates-badge]][crates-page] [![MIT licensed][mit-license-badge]][mit-license-page] [![Apache 2.0 licensed][apache-2.0-license-badge]][apache-2.0-license-page] [![Coverage][coveralls-badge]][coveralls-page]

Directory-based cache and artifact path management with discovered `.cache` roots, grouped cache paths, and optional eviction on directory initialization.

**This crate is intentionally tool-agnostic** — it only manages cache/artifact directory layout and paths and does not assume or depend on any specific consumer tooling. Any tool or library that reads or writes files can use `cache-manager` to compute/manage project-scoped cache paths and apply eviction rules.

**It has zero runtime dependencies (standard library only for library consumers).**

It is suitable for:

- Artifact storage (build outputs, generated files, intermediate data, etc.).
- Monorepos or multi-crate workspaces that need centralized cache/artifact management via a shared root (for example with `CacheRoot::from_root(...)`).

> Tested on macOS, Linux, and Windows.

## Usage

### Mental model: root -> groups -> entries

- `CacheRoot`: project/workspace anchor path.
- `CacheGroup`: subdirectory under a root where a class of cache files lives.
- Entries: files under a group (for example `v1/index.bin`).

`CacheRoot` and `CacheGroup` are lightweight path objects. Constructing them does not create directories.

### Quick start

Using `touch` (convenient when you want this crate to create the file):

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts/json");

// Create the group directory if needed.
group.ensure_dir().expect("ensure group");

// `index.bin` is just an example artifact filename that another program might generate.
let entry: std::path::PathBuf = group.touch("v1/index.bin").expect("touch entry");
println!("{}", entry.display());
```

Without `touch` (compute from `group.path()` and write with your own I/O):

```rust
use cache_manager::CacheRoot;
use std::fs;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts/json");

group.ensure_dir().expect("ensure group");

let entry_without_touch = group.path().join("v1/index.bin");
fs::create_dir_all(entry_without_touch.parent().expect("entry parent"))
	.expect("create entry parent");
fs::write(&entry_without_touch, b"artifact bytes").expect("write artifact");
println!("{}", entry_without_touch.display());
```

### Filesystem effects

- **Pure path operations:** `CacheRoot::from_root`, `CacheRoot::cache_path`, `CacheRoot::group`, `CacheGroup::entry_path`, `CacheGroup::subgroup`
- **Discovery helper (cwd/crate-root based):** `CacheRoot::from_discovery`
- **Create dirs:** `CacheRoot::ensure_group`, `CacheGroup::ensure_dir`
- **Create dirs + optional eviction:** `CacheRoot::ensure_group_with_policy`, `CacheGroup::ensure_dir_with_policy`
- **Create file (creates parents):** `CacheGroup::touch`

> Note: eviction only runs when you pass a policy to the `*_with_policy` methods.

### Discovering cache paths

Discover a cache path for the current crate/workspace and resolve an entry path.

> Note: `CacheRoot::from_discovery()?.cache_path(...)` only computes a filesystem path — it does not create directories or files.

Behavior:

- Searches upward from the current working directory for a `Cargo.toml` and uses `<crate-root>/.cache` when found; otherwise it falls back to `<cwd>/.cache`.
- The discovered anchor (`crate root` or `cwd`) is canonicalized when possible to avoid surprising
  differences between logically-equal paths.
- If the `relative_path` argument is absolute, it is returned unchanged.

```rust
use cache_manager::CacheRoot;
use std::path::Path;

// Compute a path like <crate-root>/.cache/tool/data.bin without creating it.
let cache_path = CacheRoot::from_discovery()
	.expect("discover cache root")
	.cache_path("tool", "data.bin");
println!("cache path: {}", cache_path.display());
// Expected relative location under the discovered crate root:
assert!(cache_path.ends_with(Path::new(".cache").join("tool").join("data.bin")));
// The call only computes the path; it does not create files or directories.
assert!(!cache_path.exists());

// If you already have an absolute entry path, it's returned unchanged:
let absolute = std::path::PathBuf::from("/tmp/custom/cache.json");
let kept = CacheRoot::from_discovery()
	.expect("discover cache root")
	.cache_path("tool", &absolute);
assert_eq!(kept, absolute);
```

**Notes on discovery behavior**

`CacheRoot::from_discovery()` deterministically anchors discovered cache
paths under the configured `CACHE_DIR_NAME` (default: `.cache`). It does
not scan for arbitrary directory names — creating a directory named
`.cache-v2` at the crate root will not cause `from_discovery()` to use it.
If you want to use a custom cache root, construct it explicitly with
`CacheRoot::from_root(...)`.


### Eviction Policy

Use `EvictPolicy` with:

- `CacheGroup::ensure_dir_with_policy(...)`
- `CacheRoot::ensure_group_with_policy(...)`
- `CacheGroup::eviction_report(...)` to preview which files would be evicted.

Apply policy directly to a `CacheGroup`:

```rust
use cache_manager::{CacheRoot, EvictPolicy};

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts");

let policy = EvictPolicy {
	max_files: Some(100),
	..Default::default()
};

group
	.ensure_dir_with_policy(Some(&policy))
	.expect("ensure and evict");
```

Apply policy through `CacheRoot` convenience API:

```rust
use cache_manager::{CacheRoot, EvictPolicy};
use std::time::Duration;

let root = CacheRoot::from_root("/tmp/project");
let policy = EvictPolicy {
	max_age: Some(Duration::from_secs(60 * 60 * 24 * 30)), // 30 days
	..Default::default()
};

root
	.ensure_group_with_policy("artifacts", Some(&policy))
	.expect("ensure group and evict");
```

Preview evictions without deleting files:

```rust
use cache_manager::{CacheRoot, EvictPolicy};

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts");
let policy = EvictPolicy {
	max_bytes: Some(10_000_000),
	..Default::default()
};

let report = group.eviction_report(&policy).expect("eviction report");
for path in report.marked_for_eviction {
	println!("would remove: {}", path.display());
}
```

Policy fields:

- `max_age`: remove files older than or equal to the age threshold.
- `max_files`: keep at most N files.
- `max_bytes`: keep total file bytes at or below the threshold.

Policies can be combined by setting multiple fields in one `EvictPolicy`.
When combined, all configured limits are enforced in order.

```rust
use cache_manager::EvictPolicy;
use std::time::Duration;

let combined = EvictPolicy {
	max_age: Some(Duration::from_secs(60 * 60 * 24 * 30)), // 30 days
	max_files: Some(200),
	max_bytes: Some(500 * 1024 * 1024), // 500 MB
};
```

Eviction order is always:

1. `max_age`
2. `max_files`
3. `max_bytes`

For `max_files` and `max_bytes`, files are evicted oldest-first by modified time (ascending), then by path for deterministic tie-breaking.

`eviction_report(...)` and `ensure_*_with_policy(...)` use the same selection logic.

#### How `max_bytes` works

- Scans regular files recursively under the managed directory.
- Sums `metadata.len()` across those files.
- If total exceeds `max_bytes`, removes files oldest-first until total is `<= max_bytes`.
- Directories are not counted as bytes.
- Enforcement happens only during policy-aware `ensure_*_with_policy` calls (not continuously in the background).

### Additional examples

Create or update a cache entry (ensures parent directories exist):

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_root("/tmp/project");
let group = root.group("artifacts/json");

let entry = group.touch("v1/index.bin").expect("touch entry");
println!("touched: {}", entry.display());
```

#### Per-subdirectory policies

Different subdirectories under the same `CacheRoot` can use independent policies; call `ensure_dir_with_policy` on each `CacheGroup` separately to apply per-group rules.

Note: calling `CacheGroup::ensure_dir()` is equivalent to `CacheGroup::ensure_dir_with_policy(None)`. Likewise, `CacheRoot::ensure_group(...)` behaves the same as `CacheRoot::ensure_group_with_policy(..., None)`.

#### Get the root path

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
