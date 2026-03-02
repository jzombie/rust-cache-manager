#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
/// Use `CacheRoot::discover()` to find the nearest crate root from the
/// current working directory, or `CacheRoot::from_root(...)` to construct one
/// from an explicit path.
pub struct CacheRoot {
    root: PathBuf,
}

impl CacheRoot {
    /// Discover the cache root by searching parent directories for `Cargo.toml`.
    ///
    /// Falls back to the current working directory when no crate root is found.
    pub fn discover() -> io::Result<Self> {
        let cwd = env::current_dir()?;
        let root = find_crate_root(&cwd).unwrap_or(cwd);
        // Prefer a canonicalized path when possible to avoid surprising
        // differences between logically-equal paths (symlinks, tempdir
        // representations, etc.) used by callers and tests.
        let root = root.canonicalize().unwrap_or(root);
        Ok(Self { root })
    }

    /// Create a `CacheRoot` from an explicit filesystem path.
    pub fn from_root<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    /// Like `discover()` but never returns an `io::Result` — falls back to `.` on error.
    pub fn discover_or_cwd() -> Self {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = find_crate_root(&cwd).unwrap_or(cwd);
        let root = root.canonicalize().unwrap_or(root);
        Self { root }
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
        let group = self.group_path(relative_group);
        fs::create_dir_all(&group)?;
        Ok(group)
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

    /// Discover the crate root (or use cwd) and resolve a cache entry path.
    ///
    /// Convenience wrapper for `CacheRoot::discover_or_cwd().cache_path(...)`.
    pub fn discover_cache_path<P: AsRef<Path>, Q: AsRef<Path>>(
        cache_dir: P,
        relative_path: Q,
    ) -> PathBuf {
        Self::discover_or_cwd().cache_path(cache_dir, relative_path)
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
        fs::create_dir_all(&self.path)?;
        Ok(&self.path)
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

fn find_crate_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join("Cargo.toml").is_file() {
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
    use tempfile::TempDir;

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn swap_to(path: &Path) -> io::Result<Self> {
            let previous = env::current_dir()?;
            // Try to switch to `path`. If that fails, attempt to canonicalize
            // the path and try again (helps on platforms where the tempdir
            // representation differs or when symlinks are involved).
            match env::set_current_dir(path) {
                Ok(()) => Ok(Self { previous }),
                Err(e) => {
                    if let Ok(canon) = path.canonicalize() {
                        env::set_current_dir(&canon)?;
                        Ok(Self { previous })
                    } else {
                        Err(e)
                    }
                }
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.previous);
        }
    }

    #[test]
    fn discover_falls_back_to_cwd_when_no_cargo_toml() {
        let tmp = TempDir::new().expect("tempdir");
        let _guard = CwdGuard::swap_to(tmp.path()).expect("set cwd");

        let cache = CacheRoot::discover().expect("discover");
        let got = cache
            .path()
            .canonicalize()
            .expect("canonicalize discovered root");
        let expected = tmp.path().canonicalize().expect("canonicalize temp path");
        assert_eq!(got, expected);
    }

    #[test]
    fn discover_prefers_nearest_crate_root() {
        let tmp = TempDir::new().expect("tempdir");
        let crate_root = tmp.path().join("workspace");
        let nested = crate_root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write cargo");

        let _guard = CwdGuard::swap_to(&nested).expect("set cwd");
        let cache = CacheRoot::discover().expect("discover");
        let got = cache
            .path()
            .canonicalize()
            .expect("canonicalize discovered root");
        let expected = crate_root.canonicalize().expect("canonicalize crate root");
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
    fn discover_cache_path_uses_root_and_group() {
        let tmp = TempDir::new().expect("tempdir");
        let crate_root = tmp.path().join("workspace");
        let nested = crate_root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("write cargo");

        let _guard = CwdGuard::swap_to(&nested).expect("set cwd");
        let p = CacheRoot::discover_cache_path(".cache", "taxonomy/taxonomy_cache.json");
        let parent = p.parent().expect("cache path parent");
        fs::create_dir_all(parent).expect("create cache parent");
        // Ensure the expected (non-canonicalized) parent path also exists
        // so canonicalization succeeds on platforms where temporary paths
        // may differ from the discovered/canonicalized root.
        let expected_dir = crate_root.join(".cache").join("taxonomy");
        fs::create_dir_all(&expected_dir).expect("create expected cache parent");
        let got_parent = p
            .parent()
            .expect("cache path parent")
            .canonicalize()
            .expect("canonicalize cache parent");
        let expected_parent = crate_root
            .join(".cache")
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
    fn cache_path_preserves_absolute_paths() {
        let root = CacheRoot::from_root("/tmp/project");
        let absolute = PathBuf::from("/tmp/custom/cache.json");
        let resolved = root.cache_path(".cache", &absolute);
        assert_eq!(resolved, absolute);
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
}
