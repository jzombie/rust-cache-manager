# cache-manager

[![made-with-rust][rust-logo]][rust-src-page] [![crates.io][crates-badge]][crates-page] [![MIT licensed][mit-license-badge]][mit-license-page] [![Apache 2.0 licensed][apache-2.0-license-badge]][apache-2.0-license-page] [![Coverage][coveralls-badge]][coveralls-page]

Directory-based cache and artifact path management with discovered `.cache` roots, grouped cache paths, and optional eviction on directory initialization.

- **Core capabilities**
	- **Tool-agnostic:** any tool or library that can write to the filesystem can use `cache-manager` as a managed cache/artifact path layout layer.
	- **Zero default runtime dependencies:** the standard install uses only the Rust standard library _(optional features do add additional dependencies)_.
	- **Built-in eviction policies:** enforce cache limits by file age, file count, and total bytes, with deterministic oldest-first trimming.
	- **Predictable discovery + root control:** discover `<crate-root>/.cache` automatically or pin an explicit root with `CacheRoot::from_root(...)`.
	- **Composable cache layout API:** create groups/subgroups and entry paths consistently across tools without custom path-joining logic.
	- **Artifact-friendly:** suitable for build outputs, generated files, and intermediate data.
	- **Workspace-friendly:** suitable for monorepos or multi-crate workspaces that need centralized cache/artifact management via a shared root (for example with `CacheRoot::from_root(...)`). _This tool was designed to facilitate common cache directory management in a multi-crate workspace._

- **Optional features**
	- **`process-scoped-cache`:** adds [`tempfile`](https://docs.rs/tempfile) and enables process/thread scoped caches.
	  - [`CacheRoot::from_tempdir(...)`](#cacheroot-from-tempdir)
	  - [`ProcessScopedCacheGroup::new(...)`](#processscopedcachegroup-from-root-and-group-path)
	  - [`ProcessScopedCacheGroup::from_group(...)`](#processscopedcachegroup-from-existing-group)
	- **`os-cache-dir`:** adds [`directories`](https://docs.rs/directories) and enables OS-native per-user cache roots.
	  - [`CacheRoot::from_project_dirs(...)`](#os-native-user-cache-root-optional)

- **Licensing**
	- **Open-source + commercial-friendly:** dual-licensed under [MIT][mit-license-page] or [Apache-2.0][apache-2.0-license-page].

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
use cache_manager::{CacheGroup, CacheRoot};

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: CacheGroup = root.group("artifacts/json");

// Create the group directory if needed
group.ensure_dir().expect("ensure group");

// `index.bin` is just an example artifact filename that another program might generate
let entry: std::path::PathBuf = group.touch("v1/index.bin").expect("touch entry");

let expected: std::path::PathBuf = root
	.path()
	.join("artifacts")
	.join("json")
	.join("v1")
	.join("index.bin");
assert_eq!(entry, expected);

// Example output path
println!("{}", entry.display());
```

Without `touch` (compute the path for a separate tool, then write with your own I/O):

```rust
use cache_manager::{CacheGroup, CacheRoot};
use std::fs;

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: CacheGroup = root.group("artifacts/json");

group.ensure_dir().expect("ensure group");

// This is the path you can hand to another tool/process
let entry_without_touch: std::path::PathBuf = group.entry_path("v1/index.bin");

let expected: std::path::PathBuf = root
	.path()
	.join("artifacts")
	.join("json")
	.join("v1")
	.join("index.bin");
assert_eq!(entry_without_touch, expected);

fs::create_dir_all(entry_without_touch.parent().expect("entry parent"))
	.expect("create entry parent");
fs::write(&entry_without_touch, b"artifact bytes").expect("write artifact");
println!("{}", entry_without_touch.display());
```

### Filesystem effects

- **Core APIs (always available):**
	- `CacheRoot::from_root`, `CacheRoot::from_discovery`, `CacheRoot::cache_path`, `CacheRoot::group`
	- `CacheGroup::subgroup`, `CacheGroup::entry_path`
	- `CacheRoot::ensure_group`, `CacheGroup::ensure_dir`
	- `CacheRoot::ensure_group_with_policy`, `CacheGroup::ensure_dir_with_policy`
	- `CacheGroup::touch`

- **Feature `os-cache-dir`:**
	- `CacheRoot::from_project_dirs`

- **Feature `process-scoped-cache`:**
	- `CacheRoot::from_tempdir`
	- `ProcessScopedCacheGroup::new`, `ProcessScopedCacheGroup::from_group`
	- `ProcessScopedCacheGroup::thread_group`, `ProcessScopedCacheGroup::ensure_thread_group`
	- `ProcessScopedCacheGroup::thread_entry_path`, `ProcessScopedCacheGroup::touch_thread_entry`

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

// Compute a path like <crate-root>/.cache/tool/data.bin without creating it
let cache_path: std::path::PathBuf = CacheRoot::from_discovery()
	.expect("discover cache root")
	.cache_path("tool", "data.bin");
println!("cache path: {}", cache_path.display());

// Expected relative location under the discovered crate root:
assert!(cache_path.ends_with(Path::new(".cache").join("tool").join("data.bin")));

// The call only computes the path; it does not create files or directories
assert!(!cache_path.exists());

// If you already have an absolute entry path, it's returned unchanged:
let absolute: std::path::PathBuf = std::path::PathBuf::from("/tmp/custom/cache.json");
let kept: std::path::PathBuf = CacheRoot::from_discovery()
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

### OS-native user cache root (optional)

Enable feature flag:

```bash
cargo add cache-manager --features os-cache-dir
```

Then construct a `CacheRoot` from platform-native user cache directories:

```rust
use cache_manager::CacheRoot;

let root = CacheRoot::from_project_dirs("com", "ExampleOrg", "ExampleApp")
	.expect("discover OS cache dir");

let group = root.group("artifacts");
group.ensure_dir().expect("ensure group");
```

`from_project_dirs` uses `directories::ProjectDirs` and typically resolves to:

- macOS: `~/Library/Caches/<app>`
- Linux: `$XDG_CACHE_HOME/<app>` or `~/.cache/<app>`
- Windows: `%LOCALAPPDATA%\\<org>\\<app>\\cache`

`from_project_dirs(qualifier, organization, application)` parameters:

- `qualifier`: a DNS-like namespace component (commonly `"com"` or `"org"`)
- `organization`: vendor/team name (for example `"ExampleOrg"`)
- `application`: app/tool identifier (for example `"ExampleApp"`)

Example identity tuple:

```rust
use cache_manager::CacheRoot;
use directories::ProjectDirs;
use std::fs;

let root: CacheRoot = CacheRoot::from_project_dirs("com", "Acme", "WidgetTool")
	.expect("discover OS cache dir");
let got: std::path::PathBuf = root.path().to_path_buf();

let expected: std::path::PathBuf = ProjectDirs::from("com", "Acme", "WidgetTool")
	.expect("resolve project dirs")
	.cache_dir()
	.to_path_buf();

assert_eq!(got, expected);

// If the example writes anything, keep it scoped and remove it explicitly.
let example_group = root.group("cache-manager-readme-example");
let probe = example_group.touch("probe.txt").expect("write probe");
assert!(probe.exists());
fs::remove_dir_all(example_group.path()).expect("cleanup example group");
```


### Eviction Policy

Use `EvictPolicy` with:

- `CacheGroup::ensure_dir_with_policy(...)`
- `CacheRoot::ensure_group_with_policy(...)`
- `CacheGroup::eviction_report(...)` to preview which files would be evicted.

Apply policy directly to a `CacheGroup`:

```rust
use cache_manager::{CacheRoot, EvictPolicy};

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: cache_manager::CacheGroup = root.group("artifacts");

let policy: EvictPolicy = EvictPolicy {
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

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let policy: EvictPolicy = EvictPolicy {
	max_age: Some(Duration::from_secs(60 * 60 * 24 * 30)), // 30 days
	..Default::default()
};

root
	.ensure_group_with_policy("artifacts", Some(&policy))
	.expect("ensure group and evict");
```

Preview evictions without deleting files:

```rust
use cache_manager::{CacheRoot, EvictPolicy, EvictionReport};

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: cache_manager::CacheGroup = root.group("artifacts");
let policy: EvictPolicy = EvictPolicy {
	max_bytes: Some(10_000_000),
	..Default::default()
};

let report: EvictionReport = group.eviction_report(&policy).expect("eviction report");
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

let combined: EvictPolicy = EvictPolicy {
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

### Optional process/thread scoped caches

Enable feature flag:

```bash
cargo add cache-manager --features process-scoped-cache
```

Or, if editing `Cargo.toml` manually:

```toml
[dependencies]
cache-manager = { version = "<latest>", features = ["process-scoped-cache"] }
```

#### CacheRoot from tempdir

Create a temporary cache root backed by a persisted temp directory:

```rust
#[cfg(feature = "process-scoped-cache")]
fn example_temp_root() {
	let root = cache_manager::CacheRoot::from_tempdir().expect("temp cache root");
	let group = root.group("artifacts");
	group.ensure_dir().expect("ensure group");

	// `from_tempdir` intentionally persists the directory; clean up when done.
	std::fs::remove_dir_all(root.path()).expect("cleanup temp root");
}
```

#### ProcessScopedCacheGroup from root and group path

Use this constructor when you have a `CacheRoot` plus a relative group path.
It creates a process-scoped directory under `root.group(...)`.

```rust
#[cfg(feature = "process-scoped-cache")]
fn main() {
	use cache_manager::{CacheGroup, CacheRoot, ProcessScopedCacheGroup};
	use std::path::Path;

	// 1) Build the root and the base group where process directories will live
	let root: CacheRoot = CacheRoot::from_root("/tmp/project");
	let base_group: CacheGroup = root.group("artifacts/session");

	// 2) Create a process-scoped directory (name starts with `pid-<pid>-...`)
	let scoped: ProcessScopedCacheGroup = ProcessScopedCacheGroup::new(&root, "artifacts/session")
		.expect("create process-scoped cache");

	// 3) Resolve this thread's subgroup and touch an entry under it
	let thread_group: CacheGroup = scoped.ensure_thread_group().expect("ensure thread group");
	let entry: std::path::PathBuf = thread_group.touch("v1/index.bin").expect("touch thread entry");

	// 4) Verify the static pieces of the structure
	assert!(entry.starts_with(base_group.path()));
	assert!(entry.ends_with(Path::new("v1/index.bin")));

	// 5) Verify the dynamic thread segment (`thread-<n>`)
	let thread_dir: &Path = entry
		.parent()
		.and_then(|p| p.parent())
		.expect("thread dir");

	assert!(thread_dir
		.file_name()
		.and_then(|s| s.to_str())
		.expect("thread dir name")
		.starts_with("thread-"));

	// 6) Verify the dynamic process segment (`pid-<current-pid>-<random>`)
	let process_dir: &Path = thread_dir.parent().expect("process dir");
	let expected_pid_prefix: String = format!("pid-{}-", std::process::id());

	assert!(process_dir
		.file_name()
		.and_then(|s| s.to_str())
		.expect("process dir name")
		.starts_with(&expected_pid_prefix));

	// Example output path
	println!("{}", entry.display());
}

#[cfg(not(feature = "process-scoped-cache"))]
fn main() {}
```

#### ProcessScopedCacheGroup from existing group

Use this constructor when you already have a `CacheGroup` (for example,
shared or precomputed by higher-level setup) and want process scoping from
that existing group.

```rust
#[cfg(feature = "process-scoped-cache")]
fn from_group_example() {
	use cache_manager::{CacheGroup, CacheRoot, ProcessScopedCacheGroup};

	let root: CacheRoot = CacheRoot::from_root("/tmp/project");
	let base_group: CacheGroup = root.group("artifacts/session");

	let scoped: ProcessScopedCacheGroup =
		ProcessScopedCacheGroup::from_group(base_group).expect("create process-scoped cache");
	let thread_entry = scoped
		.touch_thread_entry("v1/index.bin")
		.expect("touch thread entry");

	assert!(thread_entry.starts_with(scoped.path()));
}
```

Behavior notes:

- Respects all configured roots/groups because process-scoped paths are always created under your provided `CacheRoot`/`CacheGroup`.
- The process subdirectory is deleted when the handle is dropped during normal process shutdown.
- Cleanup is best-effort; abnormal termination (for example `SIGKILL` or crash) can leave stale directories.

### Additional examples

Create or update a cache entry (ensures parent directories exist):

```rust
use cache_manager::{CacheGroup, CacheRoot};

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: CacheGroup = root.group("artifacts/json");

let entry: std::path::PathBuf = group.touch("v1/index.bin").expect("touch entry");

let expected: std::path::PathBuf = root
	.path()
	.join("artifacts")
	.join("json")
	.join("v1")
	.join("index.bin");
assert_eq!(entry, expected);

println!("touched: {}", entry.display());
```

#### Per-subdirectory policies

Different subdirectories under the same `CacheRoot` can use independent policies; call `ensure_dir_with_policy` on each `CacheGroup` separately to apply per-group rules.

Note: calling `CacheGroup::ensure_dir()` is equivalent to `CacheGroup::ensure_dir_with_policy(None)`. Likewise, `CacheRoot::ensure_group(...)` behaves the same as `CacheRoot::ensure_group_with_policy(..., None)`.

#### Get the root path

To obtain the underlying filesystem path for a `CacheRoot`, use `path()`:

```rust
use cache_manager::CacheRoot;

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let root_path: &std::path::Path = root.path();
println!("root path: {}", root_path.display());
```

Also obtain a `CacheGroup` path and resolve an entry path under that group:

```rust
use cache_manager::{CacheGroup, CacheRoot};

let root: CacheRoot = CacheRoot::from_root("/tmp/project");
let group: CacheGroup = root.group("artifacts");

let group_path: &std::path::Path = group.path();
println!("group path: {}", group_path.display());

let entry_path: std::path::PathBuf = group.entry_path("v1/index.bin");
println!("entry path: {}", entry_path.display());
```

## License

`cache-manager` is primarily distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-APACHE][apache-2.0-license-page] and [LICENSE-MIT][mit-license-page] for details.

[rust-src-page]: https://www.rust-lang.org/
[rust-logo]: https://img.shields.io/badge/Made%20with-Rust-black

[crates-page]: https://crates.io/crates/cache-manager
[crates-badge]: https://img.shields.io/crates/v/cache-manager.svg

[mit-license-page]: https://raw.githubusercontent.com/jzombie/rust-cache-manager/refs/heads/main/LICENSE-MIT
[mit-license-badge]: https://img.shields.io/badge/license-MIT-blue.svg

[apache-2.0-license-page]: https://raw.githubusercontent.com/jzombie/rust-cache-manager/refs/heads/main/LICENSE-APACHE
[apache-2.0-license-badge]: https://img.shields.io/badge/license-Apache%202.0-blue.svg

[coveralls-page]: https://coveralls.io/github/jzombie/rust-cache-manager?branch=main
[coveralls-badge]: https://img.shields.io/coveralls/github/jzombie/rust-cache-manager
