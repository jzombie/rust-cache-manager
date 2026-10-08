//! Normalize-on-ingest permission tests: archives ship read-only modes
//! (`0444` files are live evidence from release tarballs); the cache must
//! make them owner-writable before any post-extract step touches them.
//!
//! Fixtures use `set_readonly(true)` (portable: works on Unix and Windows)
//! plus Unix-only `0o444`/`0o555` modes where the distinction matters.
//! Mode assertions are `#[cfg(unix)]`; behavior assertions
//! (writable-after, content-intact, counts) run everywhere.

use cache_manager::{CacheRoot, WritableReport, ensure_writable_tree};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fixture_tree(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
    // sub/ (r-x) / file (r--) / nested file (r--).
    let sub = base.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let file = sub.join("libcapstone.5.dylib");
    fs::write(&file, b"fake-dylib-bytes").unwrap();
    let top = base.join("README.portable");
    fs::write(&top, b"readme").unwrap();

    set_readonly(&sub, true);
    set_readonly(&file, true);
    set_readonly(&top, true);
    (sub, file, top)
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(readonly);
    fs::set_permissions(path, permissions).unwrap();
}

fn assert_writable(path: &Path) {
    let permissions = fs::metadata(path).unwrap().permissions();
    assert!(
        !permissions.readonly(),
        "{} must be writable after normalize",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            permissions.mode() & 0o200 != 0,
            "{} must carry owner-write after normalize",
            path.display()
        );
    }
}

#[test]
fn read_only_tree_becomes_writable_with_counts() {
    let tmp = TempDir::new().unwrap();
    let (sub, file, top) = fixture_tree(tmp.path());

    let report: WritableReport = ensure_writable_tree(tmp.path()).unwrap();

    assert_writable(&sub);
    assert_writable(&file);
    assert_writable(&top);
    // Root dir itself was already writable (fresh tempdir): only the three
    // read-only entries count.
    assert_eq!(report.files_fixed, 2);
    assert_eq!(report.dirs_fixed, 1);

    // Content and structure survive byte-identical.
    assert_eq!(fs::read(&file).unwrap(), b"fake-dylib-bytes");
    assert_eq!(fs::read(&top).unwrap(), b"readme");
}

#[test]
fn ensure_writable_tree_clears_readonly_and_reports_fixed_counts() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("tree");
    fs::create_dir_all(root.join("subdir")).expect("mkdir");

    let file_path = root.join("subdir/file.txt");
    fs::write(&file_path, b"data").expect("write");

    // Set read-only mode on file and directory
    let mut file_perm = fs::metadata(&file_path).expect("meta").permissions();
    file_perm.set_readonly(true);
    fs::set_permissions(&file_path, file_perm).expect("set readonly file");

    let mut dir_perm = fs::metadata(root.join("subdir"))
        .expect("meta")
        .permissions();
    dir_perm.set_readonly(true);
    fs::set_permissions(root.join("subdir"), dir_perm).expect("set readonly dir");

    // First pass: must fix 1 file and 1 directory
    let report = ensure_writable_tree(&root).expect("ensure_writable");
    assert_eq!(report.files_fixed, 1);
    assert_eq!(report.dirs_fixed, 1);
    assert!(
        !fs::metadata(&file_path)
            .expect("meta")
            .permissions()
            .readonly()
    );

    // Idempotency pass: zero fixes on second run
    let report_second = ensure_writable_tree(&root).expect("ensure_writable second");
    assert_eq!(report_second, WritableReport::default());
}

#[test]
fn other_mode_bits_are_preserved() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("exec-tool");
    fs::write(&file, b"x").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Read + exec everywhere, write nowhere: only owner-write may return.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).unwrap();

        ensure_writable_tree(tmp.path()).unwrap();

        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "only owner-write may change, got {mode:o}");
    }
    #[cfg(not(unix))]
    {
        set_readonly(&file, true);
        ensure_writable_tree(tmp.path()).unwrap();
        assert_writable(&file);
    }
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed_nor_modified() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let tmp = TempDir::new().unwrap();
    // Outside target with read-only content: the walk must not touch it.
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let target = outside.join("secret");
    fs::write(&target, b"secret").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();

    let tree = tmp.path().join("tree");
    fs::create_dir_all(&tree).unwrap();
    symlink(&target, tree.join("link")).unwrap();
    symlink(&outside, tree.join("dirlink")).unwrap();

    ensure_writable_tree(&tree).unwrap();

    // Links are still links, targets still read-only.
    assert!(
        fs::symlink_metadata(tree.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(tree.join("dirlink"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o444
    );
    assert_eq!(fs::read(&target).unwrap(), b"secret");
}

#[cfg(unix)]
#[test]
fn symlinked_root_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir_all(&real).unwrap();

    std::os::unix::fs::symlink(&real, tmp.path().join("linkroot")).unwrap();
    let err = ensure_writable_tree(&tmp.path().join("linkroot")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn single_file_root_is_fixed_in_place() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("lone-dylib");
    fs::write(&file, b"lone").unwrap();
    set_readonly(&file, true);

    let report = ensure_writable_tree(&file).unwrap();

    assert_writable(&file);
    assert_eq!(report.files_fixed, 1);
    assert_eq!(report.dirs_fixed, 0);
    assert_eq!(fs::read(&file).unwrap(), b"lone");
}

#[cfg(unix)]
#[test]
fn search_less_directory_is_traversable_after() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    // 0o444 dir: read-only AND untraversable. Owner-write alone (0o644)
    // would still EACCES the walk; only full 0o700 unblocks it.
    let sub = tmp.path().join("locked");
    fs::create_dir_all(&sub).unwrap();
    let inner = sub.join("inner");
    fs::write(&inner, b"inner").unwrap();
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o444)).unwrap();

    let report = ensure_writable_tree(tmp.path()).unwrap();

    assert_eq!(report.dirs_fixed, 1);
    assert_eq!(report.files_fixed, 1);
    // 0o444 | 0o700 == 0o744: owner rwx present, pre-existing read bits
    // preserved. The requirement is the mask, not the exact mode.
    let mode = fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode & 0o700, 0o700, "owner rwx required, got {mode:o}");
    assert_eq!(fs::read(&inner).unwrap(), b"inner");
}

#[test]
fn ensure_dir_normalizes_on_creation() {
    // Ownership at creation: group.ensure_dir() leaves the subtree
    // owner-writable, and a later re-ensure sweeps modes tar wrote
    // per-member during unpack.
    let tmp = TempDir::new().unwrap();
    let cache = CacheRoot::from_root(tmp.path());
    let group = cache.group("artifacts");
    group.ensure_dir().unwrap();

    let probe = group.entry_path("probe.bin");
    fs::write(&probe, b"x").unwrap();
    set_readonly(&probe, true);

    group.ensure_dir().unwrap();
    assert_writable(&probe);
    assert_eq!(fs::read(&probe).unwrap(), b"x");
}

#[test]
fn missing_root_errors_loudly() {
    let tmp = TempDir::new().unwrap();
    let err = ensure_writable_tree(&tmp.path().join("does-not-exist")).unwrap_err();
    assert!(
        format!("{err}").contains("does-not-exist"),
        "error must name the path: {err}"
    );
}

/// Incident reproduction (macOS only): a `0444` file rejects `xattr -dr`
/// with EACCES exactly like the release-tarball dylibs did; after
/// normalize, the same strip succeeds. This is the failure the crate
/// exists to prevent — the test fails if normalization ever regresses.
#[cfg(target_os = "macos")]
#[test]
fn quarantine_strip_succeeds_after_normalize() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("libcapstone.5.dylib");
    fs::write(&file, b"fake-dylib-bytes").unwrap();
    // Tag it while writable, then lock modes: end state matches a
    // quarantined 0444 tarball member exactly.
    let tagged = Command::new("xattr")
        .args(["-w", "com.apple.quarantine", "0000"])
        .arg(&file)
        .output()
        .expect("xattr present on macOS");
    assert!(tagged.status.success(), "tagging the fixture must succeed");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

    // Before: the strip fails with permission denied (the incident).
    let denied = Command::new("xattr")
        .args(["-d", "com.apple.quarantine", &file.to_string_lossy()])
        .output()
        .expect("xattr present on macOS");
    assert!(
        !denied.status.success(),
        "0444 fixture must reproduce the EACCES strip failure"
    );

    ensure_writable_tree(tmp.path()).unwrap();

    // After: the identical strip succeeds.
    let stripped = Command::new("xattr")
        .args(["-d", "com.apple.quarantine", &file.to_string_lossy()])
        .output()
        .expect("xattr present on macOS");
    assert!(
        stripped.status.success(),
        "strip must succeed after normalize: {}",
        String::from_utf8_lossy(&stripped.stderr)
    );
    assert!(fs::read(&file).unwrap() == b"fake-dylib-bytes");
}
