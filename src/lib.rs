#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod constants;

#[cfg(feature = "process-scoped-cache")]
use std::cell::Cell;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(feature = "process-scoped-cache")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use constants::{CACHE_DIR_NAME, CARGO_TOML_FILE_NAME};
#[cfg(feature = "os-cache-dir")]
use directories::ProjectDirs;
#[cfg(feature = "process-scoped-cache")]
use tempfile::{Builder, TempDir};

/// Optional eviction controls applied by `CacheGroup::ensure_dir_with_policy`
/// and `CacheRoot::ensure_group_with_policy`.
///
/// Rules are enforced in this order:
/// 1. `max_age` (remove files older than or equal to threshold)
/// 2. `max_files` (keep at most N files)
/// 3. `max_bytes` (keep total bytes at or below threshold)
///
/// For `max_files` and `max_bytes`, candidates are ordered by modified time
/// ascending (oldest first), then by path for deterministic tie-breaking.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EvictPolicy {
    /// Maximum number of files to keep under the managed directory tree.
    ///
    /// If exceeded, the oldest files are removed first until the count is
    /// `<= max_files`.
    pub max_files: Option<usize>,
    /// Maximum total size in bytes to keep under the managed directory tree.
    ///
    /// If exceeded, files are removed oldest-first until total bytes are
    /// `<= max_bytes`.
    ///
    /// Notes:
    /// - The limit applies to regular files recursively under the directory.
    /// - Directories are not counted toward the byte total.
    /// - Enforced only when using a policy-aware `ensure_*_with_policy` call.
    pub max_bytes: Option<u64>,
    /// Maximum file age allowed under the managed directory tree.
    ///
    /// Files with age `>= max_age` are removed.
    pub max_age: Option<Duration>,
}

/// Files selected for eviction by policy evaluation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EvictionReport {
    /// Absolute paths marked for eviction, in the order they would be applied.
    pub marked_for_eviction: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct FileEntry {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Represents a discovered or explicit cache root directory.
///
/// Use `CacheRoot::from_discovery()` to find the nearest crate root from the
/// current working directory and anchor caches under `.cache`, or
/// `CacheRoot::from_root(...)` to construct one from an explicit path.
pub struct CacheRoot {
    root: PathBuf,
}

impl CacheRoot {
    /// Discover the cache root by searching parent directories for `Cargo.toml`.
    ///
    /// The discovered cache root is always `<crate-root-or-cwd>/.cache`.
    ///
    /// Note: `from_discovery` only uses the configured `CACHE_DIR_NAME` (by
    /// default `.cache`) as the discovered cache directory. It does not
    /// detect or prefer other custom directory names (for example
    /// `.cache-v2`). To use a non-standard cache root name use
    /// `CacheRoot::from_root(...)` with an explicit path.
    ///
    /// Falls back to the current working directory when no crate root is found.
    pub fn from_discovery() -> io::Result<Self> {
        let cwd = env::current_dir()?;
        let anchor = find_crate_root(&cwd).unwrap_or(cwd);
        // Prefer a canonicalized anchor when possible to avoid surprising
        // differences between logically-equal paths (symlinks, tempdir
        // representations, etc.) used by callers and tests.
        let anchor = anchor.canonicalize().unwrap_or(anchor);
        let root = anchor.join(CACHE_DIR_NAME);
        Ok(Self { root })
    }

    /// Create a `CacheRoot` from an explicit filesystem path.
    pub fn from_root<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    /// Create a `CacheRoot` from an OS-native per-user cache directory for
    /// the given project identity.
    ///
    /// This API is available when the `os-cache-dir` feature is enabled and
    /// uses [`directories::ProjectDirs`] internally.
    ///
    /// The returned path is OS-specific and typically resolves to:
    /// - macOS: `~/Library/Caches/<app>`
    /// - Linux: `$XDG_CACHE_HOME/<app>` or `~/.cache/<app>`
    /// - Windows: `%LOCALAPPDATA%\\<org>\\<app>\\cache`
    ///
    /// Parameters are passed directly to `ProjectDirs::from(qualifier,
    /// organization, application)`.
    ///
    /// `qualifier` is a DNS-like namespace component used primarily on some
    /// platforms (notably macOS) to form a unique app identity. Common values
    /// include:
    /// - `"com"` (for apps under a `com.<org>.<app>` naming scheme)
    /// - `"org"` (for apps under an `org.<org>.<app>` naming scheme)
    ///
    /// Example:
    /// `CacheRoot::from_project_dirs("com", "Acme", "WidgetTool")`.
    #[cfg(feature = "os-cache-dir")]
    pub fn from_project_dirs(
        qualifier: &str,
        organization: &str,
        application: &str,
    ) -> io::Result<Self> {
        let project_dirs =
            ProjectDirs::from(qualifier, organization, application).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "could not resolve an OS cache directory for the provided project identity",
                )
            })?;

        Ok(Self {
            root: project_dirs.cache_dir().to_path_buf(),
        })
    }

    /// Create a `CacheRoot` using a newly-created directory under the system
    /// temporary directory.
    ///
    /// This API is available when the `process-scoped-cache` feature is
    /// enabled (which provides the `tempfile` dependency).
    ///
    /// The directory is intentionally persisted and returned as the cache root,
    /// so it is **not** automatically deleted when this function returns.
    /// Callers who want cleanup should remove `root.path()` explicitly when
    /// finished.
    #[cfg(feature = "process-scoped-cache")]
    pub fn from_tempdir() -> io::Result<Self> {
        let root = TempDir::new()?.keep();
        Ok(Self { root })
    }

    /// Return the underlying path for this `CacheRoot`.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Build a `CacheGroup` for a relative subdirectory under this root.
    pub fn group<P: AsRef<Path>>(&self, relative_group: P) -> CacheGroup {
        let path = self.root.join(relative_group.as_ref());
        CacheGroup { path }
    }

    /// Resolve a relative group path to an absolute `PathBuf` under this root.
    pub fn group_path<P: AsRef<Path>>(&self, relative_group: P) -> PathBuf {
        self.root.join(relative_group.as_ref())
    }

    /// Ensure the given group directory exists, creating parents as required.
    pub fn ensure_group<P: AsRef<Path>>(&self, relative_group: P) -> io::Result<PathBuf> {
        self.ensure_group_with_policy(relative_group, None)
    }

    /// Ensure the given group exists and optionally apply an eviction policy.
    ///
    /// When `policy` is `Some`, files will be evaluated and removed according
    /// to the `EvictPolicy` rules. Passing `None` performs only directory creation.
    pub fn ensure_group_with_policy<P: AsRef<Path>>(
        &self,
        relative_group: P,
        policy: Option<&EvictPolicy>,
    ) -> io::Result<PathBuf> {
        let group = self.group(relative_group);
        group.ensure_dir_with_policy(policy)?;
        Ok(group.path().to_path_buf())
    }

    /// Resolve a cache entry path given a cache directory (relative to the root)
    /// and a relative entry path. Absolute `relative_path` values are returned
    /// unchanged.
    pub fn cache_path<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        cache_dir: P,
        relative_path: Q,
    ) -> PathBuf {
        let rel = relative_path.as_ref();
        if rel.is_absolute() {
            return rel.to_path_buf();
        }
        self.group(cache_dir).entry_path(rel)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A group (subdirectory) under a `CacheRoot` that manages cache entries.
///
/// Use `CacheRoot::group(...)` to construct a `CacheGroup` rooted under a
/// `CacheRoot`.
pub struct CacheGroup {
    path: PathBuf,
}

impl CacheGroup {
    /// Return the path of this cache group.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ensure the group directory exists on disk, creating parents as needed.
    pub fn ensure_dir(&self) -> io::Result<&Path> {
        self.ensure_dir_with_policy(None)
    }

    /// Ensures this directory exists, then applies optional eviction.
    ///
    /// Eviction is applied recursively to files under this directory. The
    /// policy is best-effort for removals: individual delete failures are
    /// ignored so initialization can continue.
    pub fn ensure_dir_with_policy(&self, policy: Option<&EvictPolicy>) -> io::Result<&Path> {
        fs::create_dir_all(&self.path)?;
        if let Some(policy) = policy {
            apply_evict_policy(&self.path, policy)?;
        }
        Ok(&self.path)
    }

    /// Returns a report of files that would be evicted under `policy`.
    ///
    /// This does not delete files. The selection order matches the internal
    /// order used by `ensure_dir_with_policy`.
    /// Return a report of files that would be evicted by `policy`.
    ///
    /// The report is non-destructive and mirrors the selection used by
    /// `ensure_dir_with_policy` so it can be used for previewing or testing.
    pub fn eviction_report(&self, policy: &EvictPolicy) -> io::Result<EvictionReport> {
        build_eviction_report(&self.path, policy)
    }

    /// Create a nested subgroup under this group.
    pub fn subgroup<P: AsRef<Path>>(&self, relative_group: P) -> Self {
        Self {
            path: self.path.join(relative_group.as_ref()),
        }
    }

    /// Resolve a relative entry path under this group.
    pub fn entry_path<P: AsRef<Path>>(&self, relative_file: P) -> PathBuf {
        self.path.join(relative_file.as_ref())
    }

    /// Create or update (touch) a file under this group, creating parent
    /// directories as needed. Returns the absolute path to the entry.
    pub fn touch<P: AsRef<Path>>(&self, relative_file: P) -> io::Result<PathBuf> {
        let entry = self.entry_path(relative_file);
        if let Some(parent) = entry.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(&entry)?;
        Ok(entry)
    }
}

/// Process-scoped cache group handle with per-thread subgroup helpers.
///
/// This type is available when the `process-scoped-cache` feature is enabled.
///
/// It creates an auto-generated process subdirectory under a user-selected
/// base cache group. The backing directory is removed when this handle is
/// dropped during normal process shutdown.
///
/// Notes:
/// - Cleanup is best-effort and is not guaranteed after abnormal termination
///   (for example `SIGKILL` or process crash).
/// - All paths still respect the caller-provided `CacheRoot` and base group.
#[cfg(feature = "process-scoped-cache")]
#[derive(Debug)]
pub struct ProcessScopedCacheGroup {
    process_group: CacheGroup,
    _temp_dir: TempDir,
}

#[cfg(feature = "process-scoped-cache")]
impl ProcessScopedCacheGroup {
    /// Create a process-scoped cache handle under `root.group(relative_group)`.
    pub fn new<P: AsRef<Path>>(root: &CacheRoot, relative_group: P) -> io::Result<Self> {
        Self::from_group(root.group(relative_group))
    }

    /// Create a process-scoped cache handle under an existing base group.
    pub fn from_group(base_group: CacheGroup) -> io::Result<Self> {
        base_group.ensure_dir()?;
        let pid = std::process::id();
        let temp_dir = Builder::new()
            .prefix(&format!("pid-{pid}-"))
            .tempdir_in(base_group.path())?;
        let process_group = CacheGroup {
            path: temp_dir.path().to_path_buf(),
        };

        Ok(Self {
            process_group,
            _temp_dir: temp_dir,
        })
    }

    /// Return the process-scoped directory path.
    pub fn path(&self) -> &Path {
        self.process_group.path()
    }

    /// Return the process-scoped cache group.
    pub fn process_group(&self) -> CacheGroup {
        self.process_group.clone()
    }

    /// Return the subgroup for the current thread.
    ///
    /// Each thread gets a stable, process-local incremental id (`thread-<n>`)
    /// for the process lifetime.
    pub fn thread_group(&self) -> CacheGroup {
        self.process_group
            .subgroup(format!("thread-{}", current_thread_cache_group_id()))
    }

    /// Ensure and return the subgroup for the current thread.
    pub fn ensure_thread_group(&self) -> io::Result<CacheGroup> {
        let group = self.thread_group();
        group.ensure_dir()?;
        Ok(group)
    }

    /// Build an entry path inside the current thread subgroup.
    pub fn thread_entry_path<P: AsRef<Path>>(&self, relative_file: P) -> PathBuf {
        self.thread_group().entry_path(relative_file)
    }

    /// Touch an entry inside the current thread subgroup.
    pub fn touch_thread_entry<P: AsRef<Path>>(&self, relative_file: P) -> io::Result<PathBuf> {
        self.ensure_thread_group()?.touch(relative_file)
    }
}

#[cfg(feature = "process-scoped-cache")]
fn current_thread_cache_group_id() -> u64 {
    thread_local! {
        static THREAD_GROUP_ID: Cell<Option<u64>> = const { Cell::new(None) };
    }

    static NEXT_THREAD_GROUP_ID: AtomicU64 = AtomicU64::new(1);

    THREAD_GROUP_ID.with(|slot| {
        if let Some(id) = slot.get() {
            id
        } else {
            let id = NEXT_THREAD_GROUP_ID.fetch_add(1, Ordering::Relaxed);
            slot.set(Some(id));
            id
        }
    })
}

fn find_crate_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(CARGO_TOML_FILE_NAME).is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn apply_evict_policy(root: &Path, policy: &EvictPolicy) -> io::Result<()> {
    let report = build_eviction_report(root, policy)?;

    for path in report.marked_for_eviction {
        let _ = fs::remove_file(path);
    }

    Ok(())
}

fn sort_entries_oldest_first(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        let ta = a
            .modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let tb = b
            .modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        ta.cmp(&tb).then_with(|| a.path.cmp(&b.path))
    });
}

fn build_eviction_report(root: &Path, policy: &EvictPolicy) -> io::Result<EvictionReport> {
    let mut entries = collect_files(root)?;
    let mut marked_for_eviction = Vec::new();

    if let Some(max_age) = policy.max_age {
        let now = SystemTime::now();
        let mut survivors = Vec::with_capacity(entries.len());
        for entry in entries {
            let age = now.duration_since(entry.modified).unwrap_or(Duration::ZERO);
            if age >= max_age {
                marked_for_eviction.push(entry.path);
            } else {
                survivors.push(entry);
            }
        }
        entries = survivors;
    }

    sort_entries_oldest_first(&mut entries);

    if let Some(max_files) = policy.max_files
        && entries.len() > max_files
    {
        let to_remove = entries.len() - max_files;
        for entry in entries.iter().take(to_remove) {
            marked_for_eviction.push(entry.path.clone());
        }
        entries = entries.into_iter().skip(to_remove).collect();
        sort_entries_oldest_first(&mut entries);
    }

    if let Some(max_bytes) = policy.max_bytes {
        let mut total: u64 = entries.iter().map(|e| e.len).sum();
        if total > max_bytes {
            for entry in &entries {
                if total <= max_bytes {
                    break;
                }
                marked_for_eviction.push(entry.path.clone());
                total = total.saturating_sub(entry.len);
            }
        }
    }

    Ok(EvictionReport {
        marked_for_eviction,
    })
}

fn collect_files(root: &Path) -> io::Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    collect_files_recursive(root, &mut out)?;
    Ok(out)
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<FileEntry>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            collect_files_recursive(&path, out)?;
        } else if meta.is_file() {
            out.push(FileEntry {
                path,
                modified: meta.modified().unwrap_or(UNIX_EPOCH),
                len: meta.len(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    // Serialize tests that mutate the process working directory.
    //
    // Many unit tests temporarily call `env::set_current_dir` to exercise
    // discovery behavior. Because the process CWD is global, those tests
    // can race when run in parallel and cause flaky failures (different
    // tests observing different CWDs). We use a global `Mutex<()>` to
    // serialize CWD-changing tests so they run one at a time.
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Return the global mutex used to serialize tests that change the
    /// process current working directory. Stored in a `OnceLock` so it is
    /// initialized on first use and lives for the duration of the process.
    fn cwd_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Test helper that temporarily changes the process current working
    /// directory and restores it when dropped. While alive it also holds
    /// the global `cwd_lock()` so no two tests can race by changing the
    /// CWD concurrently.
    struct CwdGuard {
        previous: PathBuf,
        // Hold the guard so the lock remains taken for the lifetime of
        // this `CwdGuard` instance.
        _cwd_lock: MutexGuard<'static, ()>,
    }

    impl CwdGuard {
        fn swap_to(path: &Path) -> io::Result<Self> {
            // Acquire the global lock before mutating the CWD to avoid
            // races with other tests that also change the CWD.
            let cwd_lock_guard = cwd_lock().lock().expect("acquire cwd test lock");
            let previous = env::current_dir()?;
            env::set_current_dir(path)?;
            Ok(Self {
                previous,
                _cwd_lock: cwd_lock_guard,
            })
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.previous);
        }
    }

    #[test]
    fn from_discovery_uses_cwd_dot_cache_when_no_cargo_toml() {
        let tmp = TempDir::new().expect("tempdir");
        let _guard = CwdGuard::swap_to(tmp.path()).expect("set cwd");

        let cache = CacheRoot::from_discovery().expect("discover");
        let got = cache.path().to_path_buf();
        let expected = tmp
            .path()
            .canonicalize()
            .expect("canonicalize temp path")
            .join(CACHE_DIR_NAME);
        assert_eq!(got, expected);
    }

    #[test]
    fn from_discovery_prefers_nearest_crate_root() {
        let tmp = TempDir::new().expect("tempdir");
        let crate_root = tmp.path().join("workspace");
        let nested = crate_root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            crate_root.join(CARGO_TOML_FILE_NAME),
            "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write cargo");

        let _guard = CwdGuard::swap_to(&nested).expect("set cwd");
        let cache = CacheRoot::from_discovery().expect("discover");
        let got = cache.path().to_path_buf();
        let expected = crate_root
            .canonicalize()
            .expect("canonicalize crate root")
            .join(CACHE_DIR_NAME);
        assert_eq!(got, expected);
    }

    #[test]
    fn from_root_supports_arbitrary_path_and_grouping() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path().join("custom-cache-root"));
        let group = root.group("taxonomy/v1");

        assert_eq!(group.path(), root.path().join("taxonomy/v1").as_path());
    }

    #[test]
    fn group_path_building_and_dir_creation() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts/json");

        let nested_group = group.subgroup("v1");
        let ensured = nested_group.ensure_dir().expect("ensure nested dir");
        let expected_group_suffix = Path::new("artifacts").join("json").join("v1");
        assert!(ensured.ends_with(&expected_group_suffix));
        assert!(ensured.exists());

        let entry = nested_group.entry_path("a/b/cache.json");
        let expected_entry_suffix = Path::new("artifacts")
            .join("json")
            .join("v1")
            .join("a")
            .join("b")
            .join("cache.json");
        assert!(entry.ends_with(&expected_entry_suffix));
    }

    #[test]
    fn touch_creates_blank_file_and_is_idempotent() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts/json");

        let touched = group.touch("a/b/cache.json").expect("touch file");
        assert!(touched.exists());
        let meta = fs::metadata(&touched).expect("metadata");
        assert_eq!(meta.len(), 0);

        let touched_again = group.touch("a/b/cache.json").expect("touch file again");
        assert_eq!(touched_again, touched);
        let meta_again = fs::metadata(&touched_again).expect("metadata again");
        assert_eq!(meta_again.len(), 0);
    }

    #[test]
    fn touch_with_root_group_and_empty_relative_path_errors() {
        let root = CacheRoot::from_root("/");
        let group = root.group("");

        let result = group.touch("");
        assert!(result.is_err());
    }

    #[test]
    fn from_discovery_cache_path_uses_root_and_group() {
        let tmp = TempDir::new().expect("tempdir");
        let crate_root = tmp.path().join("workspace");
        let nested = crate_root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            crate_root.join(CARGO_TOML_FILE_NAME),
            "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write cargo");

        let _guard = CwdGuard::swap_to(&nested).expect("set cwd");
        let p = CacheRoot::from_discovery()
            .expect("discover")
            .cache_path("taxonomy", "taxonomy_cache.json");
        let parent = p.parent().expect("cache path parent");
        fs::create_dir_all(parent).expect("create cache parent");
        // Ensure the expected (non-canonicalized) parent path also exists
        // so canonicalization succeeds on platforms where temporary paths
        // may differ from the discovered/canonicalized root.
        let expected_dir = crate_root.join(CACHE_DIR_NAME).join("taxonomy");
        fs::create_dir_all(&expected_dir).expect("create expected cache parent");
        let got_parent = p
            .parent()
            .expect("cache path parent")
            .canonicalize()
            .expect("canonicalize cache parent");
        let expected_parent = crate_root
            .join(CACHE_DIR_NAME)
            .join("taxonomy")
            .canonicalize()
            .expect("canonicalize expected parent");
        assert_eq!(got_parent, expected_parent);
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("taxonomy_cache.json")
        );
    }

    #[test]
    fn from_discovery_ignores_other_custom_cache_dir_names() {
        let tmp = TempDir::new().expect("tempdir");
        let crate_root = tmp.path().join("workspace");
        let nested = crate_root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            crate_root.join(CARGO_TOML_FILE_NAME),
            "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write cargo");

        // Create a non-standard cache directory name at the crate root.
        fs::create_dir_all(crate_root.join(".cache-v2")).expect("create custom cache dir");

        let _guard = CwdGuard::swap_to(&nested).expect("set cwd");
        let cache = CacheRoot::from_discovery().expect("discover");

        // from_discovery should still resolve to `<crate_root>/.cache` (not `.cache-v2`).
        let expected = crate_root
            .canonicalize()
            .expect("canonicalize crate root")
            .join(CACHE_DIR_NAME);
        assert_eq!(cache.path(), expected);
    }

    #[test]
    fn cache_path_preserves_absolute_paths() {
        let root = CacheRoot::from_root("/tmp/project");
        let absolute = PathBuf::from("/tmp/custom/cache.json");
        let resolved = root.cache_path(CACHE_DIR_NAME, &absolute);
        assert_eq!(resolved, absolute);
    }

    #[cfg(feature = "os-cache-dir")]
    #[test]
    fn from_project_dirs_matches_directories_cache_dir() {
        let qualifier = "com";
        let organization = "CacheManagerTests";
        let application = "CacheManagerOsCacheRoot";

        let expected = ProjectDirs::from(qualifier, organization, application)
            .expect("project dirs")
            .cache_dir()
            .to_path_buf();

        let root = CacheRoot::from_project_dirs(qualifier, organization, application)
            .expect("from project dirs");

        assert_eq!(root.path(), expected.as_path());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn from_tempdir_creates_existing_writable_root() {
        let root = CacheRoot::from_tempdir().expect("from tempdir");
        assert!(root.path().is_dir());

        let probe_group = root.group("probe");
        let probe_file = probe_group.touch("writable.txt").expect("touch probe");
        assert!(probe_file.is_file());

        fs::remove_dir_all(root.path()).expect("cleanup temp root");
    }

    #[test]
    fn ensure_dir_with_policy_max_files() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.txt"), b"1").expect("write a");
        fs::write(group.entry_path("b.txt"), b"1").expect("write b");
        fs::write(group.entry_path("c.txt"), b"1").expect("write c");

        let policy = EvictPolicy {
            max_files: Some(2),
            ..EvictPolicy::default()
        };
        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("ensure with policy");

        let files = collect_files(group.path()).expect("collect files");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn ensure_dir_with_policy_max_bytes() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.bin"), vec![1u8; 5]).expect("write a");
        fs::write(group.entry_path("b.bin"), vec![1u8; 5]).expect("write b");
        fs::write(group.entry_path("c.bin"), vec![1u8; 5]).expect("write c");

        let policy = EvictPolicy {
            max_bytes: Some(10),
            ..EvictPolicy::default()
        };
        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("ensure with policy");

        let total: u64 = collect_files(group.path())
            .expect("collect files")
            .iter()
            .map(|f| f.len)
            .sum();
        assert!(total <= 10);
    }

    #[test]
    fn ensure_dir_with_policy_max_age_zero_evicts_all() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.txt"), b"1").expect("write a");
        fs::write(group.entry_path("b.txt"), b"1").expect("write b");

        let policy = EvictPolicy {
            max_age: Some(Duration::ZERO),
            ..EvictPolicy::default()
        };
        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("ensure with policy");

        let files = collect_files(group.path()).expect("collect files");
        assert!(files.is_empty());
    }

    #[test]
    fn eviction_report_matches_applied_evictions() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.bin"), vec![1u8; 5]).expect("write a");
        fs::write(group.entry_path("b.bin"), vec![1u8; 5]).expect("write b");
        fs::write(group.entry_path("c.bin"), vec![1u8; 5]).expect("write c");

        let policy = EvictPolicy {
            max_bytes: Some(10),
            ..EvictPolicy::default()
        };

        let before: BTreeSet<PathBuf> = collect_files(group.path())
            .expect("collect before")
            .into_iter()
            .map(|f| f.path)
            .collect();

        let report = group.eviction_report(&policy).expect("eviction report");
        let planned: BTreeSet<PathBuf> = report.marked_for_eviction.iter().cloned().collect();

        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("ensure with policy");

        let after: BTreeSet<PathBuf> = collect_files(group.path())
            .expect("collect after")
            .into_iter()
            .map(|f| f.path)
            .collect();

        let expected_after: BTreeSet<PathBuf> = before.difference(&planned).cloned().collect();
        assert_eq!(after, expected_after);
    }

    #[test]
    fn no_policy_and_default_policy_report_do_not_mark_evictions() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.txt"), b"1").expect("write a");
        fs::write(group.entry_path("b.txt"), b"1").expect("write b");

        let report = group
            .eviction_report(&EvictPolicy::default())
            .expect("eviction report");
        assert!(report.marked_for_eviction.is_empty());

        group
            .ensure_dir_with_policy(None)
            .expect("ensure with no policy");

        let files = collect_files(group.path()).expect("collect files");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn eviction_policy_applies_in_documented_order() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("old.txt"), vec![1u8; 1]).expect("write old");

        std::thread::sleep(Duration::from_millis(300));

        fs::write(group.entry_path("b.bin"), vec![1u8; 7]).expect("write b");
        fs::write(group.entry_path("c.bin"), vec![1u8; 6]).expect("write c");
        fs::write(group.entry_path("d.bin"), vec![1u8; 1]).expect("write d");

        let policy = EvictPolicy {
            max_age: Some(Duration::from_millis(200)),
            max_files: Some(2),
            max_bytes: Some(5),
        };

        let report = group.eviction_report(&policy).expect("eviction report");
        let evicted_names: Vec<String> = report
            .marked_for_eviction
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .expect("evicted file name")
                    .to_string()
            })
            .collect();

        assert_eq!(evicted_names, vec!["old.txt", "b.bin", "c.bin"]);

        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("apply policy");

        let remaining_names: BTreeSet<String> = collect_files(group.path())
            .expect("collect remaining")
            .into_iter()
            .map(|entry| {
                entry
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("remaining file name")
                    .to_string()
            })
            .collect();

        assert_eq!(remaining_names, BTreeSet::from(["d.bin".to_string()]));
    }

    #[test]
    fn sort_entries_uses_path_as_tie_break_for_equal_modified_time() {
        let same_time = UNIX_EPOCH + Duration::from_secs(1_234_567);
        let mut entries = vec![
            FileEntry {
                path: PathBuf::from("z.bin"),
                modified: same_time,
                len: 1,
            },
            FileEntry {
                path: PathBuf::from("a.bin"),
                modified: same_time,
                len: 1,
            },
            FileEntry {
                path: PathBuf::from("m.bin"),
                modified: same_time,
                len: 1,
            },
        ];

        sort_entries_oldest_first(&mut entries);

        let ordered_paths: Vec<PathBuf> = entries.into_iter().map(|entry| entry.path).collect();
        assert_eq!(
            ordered_paths,
            vec![
                PathBuf::from("a.bin"),
                PathBuf::from("m.bin"),
                PathBuf::from("z.bin")
            ]
        );
    }

    #[test]
    fn single_root_supports_distinct_policies_per_subdirectory() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());

        let images = cache.group("artifacts/images");
        let reports = cache.group("artifacts/reports");

        images.ensure_dir().expect("ensure images dir");
        reports.ensure_dir().expect("ensure reports dir");

        fs::write(images.entry_path("img1.bin"), vec![1u8; 5]).expect("write img1");
        fs::write(images.entry_path("img2.bin"), vec![1u8; 5]).expect("write img2");
        fs::write(images.entry_path("img3.bin"), vec![1u8; 5]).expect("write img3");

        fs::write(reports.entry_path("a.txt"), b"1").expect("write report a");
        fs::write(reports.entry_path("b.txt"), b"1").expect("write report b");
        fs::write(reports.entry_path("c.txt"), b"1").expect("write report c");

        let images_policy = EvictPolicy {
            max_bytes: Some(10),
            ..EvictPolicy::default()
        };
        let reports_policy = EvictPolicy {
            max_files: Some(1),
            ..EvictPolicy::default()
        };

        images
            .ensure_dir_with_policy(Some(&images_policy))
            .expect("apply images policy");
        reports
            .ensure_dir_with_policy(Some(&reports_policy))
            .expect("apply reports policy");

        let images_total: u64 = collect_files(images.path())
            .expect("collect images files")
            .iter()
            .map(|f| f.len)
            .sum();
        assert!(images_total <= 10);

        let reports_files = collect_files(reports.path()).expect("collect reports files");
        assert_eq!(reports_files.len(), 1);
    }

    #[test]
    fn group_path_and_ensure_group_create_expected_directory() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());

        let expected = tmp.path().join("a/b/c");
        assert_eq!(cache.group_path("a/b/c"), expected);

        let ensured = cache.ensure_group("a/b/c").expect("ensure group");
        assert_eq!(ensured, expected);
        assert!(ensured.is_dir());
    }

    #[test]
    fn ensure_group_with_policy_applies_eviction_rules() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());

        cache
            .ensure_group_with_policy("artifacts", None)
            .expect("ensure group without policy");

        let group = cache.group("artifacts");
        fs::write(group.entry_path("a.bin"), vec![1u8; 1]).expect("write a");
        fs::write(group.entry_path("b.bin"), vec![1u8; 1]).expect("write b");
        fs::write(group.entry_path("c.bin"), vec![1u8; 1]).expect("write c");

        let policy = EvictPolicy {
            max_files: Some(1),
            ..EvictPolicy::default()
        };

        let ensured = cache
            .ensure_group_with_policy("artifacts", Some(&policy))
            .expect("ensure group with policy");
        assert_eq!(ensured, group.path());

        let files = collect_files(group.path()).expect("collect files");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn cache_path_joins_relative_paths_under_group() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());

        let got = cache.cache_path(CACHE_DIR_NAME, "tool/v1/data.bin");
        let expected = tmp
            .path()
            .join(CACHE_DIR_NAME)
            .join("tool")
            .join("v1")
            .join("data.bin");
        assert_eq!(got, expected);
    }

    #[test]
    fn subgroup_touch_creates_parent_directories() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let subgroup = cache.group("artifacts").subgroup("json/v1");

        let touched = subgroup
            .touch("nested/output.bin")
            .expect("touch subgroup entry");

        assert!(touched.is_file());
        assert!(subgroup.path().join("nested").is_dir());
    }

    #[test]
    fn eviction_report_errors_when_group_directory_is_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let missing = cache.group("does-not-exist");

        let err = missing
            .eviction_report(&EvictPolicy::default())
            .expect_err("eviction report should fail for missing directory");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn eviction_policy_scans_nested_subdirectories_recursively() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::create_dir_all(group.entry_path("nested/deeper")).expect("create nested dirs");
        fs::write(group.entry_path("root.bin"), vec![1u8; 1]).expect("write root");
        fs::write(group.entry_path("nested/a.bin"), vec![1u8; 1]).expect("write nested a");
        fs::write(group.entry_path("nested/deeper/b.bin"), vec![1u8; 1]).expect("write nested b");

        let policy = EvictPolicy {
            max_files: Some(1),
            ..EvictPolicy::default()
        };

        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("apply recursive policy");

        let remaining = collect_files(group.path()).expect("collect remaining");
        assert_eq!(remaining.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn collect_files_recursive_ignores_non_file_non_directory_entries() {
        use std::os::unix::net::UnixListener;

        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        let socket_path = group.entry_path("live.sock");
        let _listener = UnixListener::bind(&socket_path).expect("bind unix socket");

        fs::write(group.entry_path("a.bin"), vec![1u8; 1]).expect("write file");

        let files = collect_files(group.path()).expect("collect files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, group.entry_path("a.bin"));
    }

    #[test]
    fn max_bytes_policy_under_threshold_does_not_evict() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts");
        group.ensure_dir().expect("ensure dir");

        fs::write(group.entry_path("a.bin"), vec![1u8; 2]).expect("write a");
        fs::write(group.entry_path("b.bin"), vec![1u8; 3]).expect("write b");

        let policy = EvictPolicy {
            max_bytes: Some(10),
            ..EvictPolicy::default()
        };

        let report = group.eviction_report(&policy).expect("eviction report");
        assert!(report.marked_for_eviction.is_empty());

        group
            .ensure_dir_with_policy(Some(&policy))
            .expect("ensure with policy");

        let files = collect_files(group.path()).expect("collect files");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn cwd_guard_swap_to_returns_error_for_missing_directory() {
        let tmp = TempDir::new().expect("tempdir");
        let missing = tmp.path().join("missing-dir");

        let result = CwdGuard::swap_to(&missing);
        assert!(result.is_err());
        assert_eq!(
            result.err().expect("expected missing-dir error").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn ensure_dir_equals_ensure_dir_with_policy_none() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());
        let group = cache.group("artifacts/eq");

        let p1 = group.ensure_dir().expect("ensure dir");
        // create a file so we can verify calling the policy-aware
        // variant with `None` does not remove or alter contents.
        fs::write(group.entry_path("keep.txt"), b"keep").expect("write file");

        let p2 = group
            .ensure_dir_with_policy(None)
            .expect("ensure dir with None policy");

        assert_eq!(p1, p2);
        assert!(group.entry_path("keep.txt").exists());
    }

    #[test]
    fn ensure_group_equals_ensure_group_with_policy_none() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = CacheRoot::from_root(tmp.path());

        let p1 = cache.ensure_group("artifacts/roots").expect("ensure group");
        let group = cache.group("artifacts/roots");
        // create a file to ensure no-op policy does not remove it
        fs::write(group.entry_path("keep_root.txt"), b"keep").expect("write file");

        let p2 = cache
            .ensure_group_with_policy("artifacts/roots", None)
            .expect("ensure group with None policy");

        assert_eq!(p1, p2);
        assert!(group.entry_path("keep_root.txt").exists());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_respects_root_and_group_assignments() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path().join("custom-root"));

        let scoped = ProcessScopedCacheGroup::new(&root, "artifacts/session").expect("create");
        let expected_prefix = root.group("artifacts/session").path().to_path_buf();

        assert!(scoped.path().starts_with(&expected_prefix));
        assert!(scoped.path().exists());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_deletes_directory_on_drop() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());

        let process_dir = {
            let scoped = ProcessScopedCacheGroup::new(&root, "artifacts").expect("create");
            let p = scoped.path().to_path_buf();
            assert!(p.exists());
            p
        };

        assert!(!process_dir.exists());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_thread_group_is_stable_per_thread() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());
        let scoped = ProcessScopedCacheGroup::new(&root, "artifacts").expect("create");

        let first = scoped.thread_group().path().to_path_buf();
        let second = scoped.thread_group().path().to_path_buf();

        assert_eq!(first, second);
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_thread_group_differs_across_threads() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());
        let scoped = ProcessScopedCacheGroup::new(&root, "artifacts").expect("create");

        let main_thread_group = scoped.thread_group().path().to_path_buf();
        let other_thread_group = std::thread::spawn(current_thread_cache_group_id)
            .join()
            .expect("join thread");

        let expected_other = scoped
            .process_group()
            .subgroup(format!("thread-{other_thread_group}"))
            .path()
            .to_path_buf();

        assert_ne!(main_thread_group, expected_other);
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_from_group_uses_given_base_group() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());
        let base_group = root.group("artifacts/custom-base");

        let scoped = ProcessScopedCacheGroup::from_group(base_group.clone()).expect("create");

        assert!(scoped.path().starts_with(base_group.path()));
        assert_eq!(scoped.process_group().path(), scoped.path());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn process_scoped_cache_thread_entry_path_matches_touch_location() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());
        let scoped = ProcessScopedCacheGroup::new(&root, "artifacts").expect("create");

        let planned = scoped.thread_entry_path("nested/data.bin");
        let touched = scoped
            .touch_thread_entry("nested/data.bin")
            .expect("touch thread entry");

        assert_eq!(planned, touched);
        assert!(touched.exists());
    }

    #[cfg(feature = "process-scoped-cache")]
    #[test]
    fn touch_thread_entry_creates_entry_under_thread_group() {
        let tmp = TempDir::new().expect("tempdir");
        let root = CacheRoot::from_root(tmp.path());
        let scoped = ProcessScopedCacheGroup::new(&root, "artifacts").expect("create");

        let entry = scoped
            .touch_thread_entry("nested/data.bin")
            .expect("touch thread entry");

        assert!(entry.exists());
        assert!(entry.starts_with(scoped.path()));
        let thread_group = scoped.thread_group().path().to_path_buf();
        assert!(entry.starts_with(&thread_group));
    }
}
