use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use tempfile::TempDir;

/// End-to-end integration test that verifies the crate's discovery and
/// default cache creation when a conventional binary (not `cargo`) uses
/// the library.
///
/// Steps performed:
/// 1. Create isolated temporary paths for source code and runtime.
/// 2. Write a tiny `main.rs` that calls `CacheRoot::from_discovery()`,
///    ensures a group directory, touches a probe file, and prints the
///    three resolved paths (root, group, probe) to stdout.
/// 3. Compile that binary using `rustc` and run it directly via
///    `std::process::Command` (the test intentionally avoids `cargo`).
/// 4. Run the binary from a directory with no `Cargo.toml`, then assert
///    discovery falls back to `<cwd>/.cache`, and that subgroup + probe
///    file exist and remain writable. This is cross-platform and does
///    not use any platform-specific APIs.
#[test]
fn e2e_binary_compiled_with_rustc_creates_default_cache_under_temp_root() {
    let tmp = TempDir::new().expect("tempdir");
    let source_root = tmp.path().join("source");
    let runtime_root = tmp.path().join("runtime");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::create_dir_all(&runtime_root).expect("create runtime dir");

    // Create a Cargo.toml near source to prove runtime discovery is based on
    // process cwd (runtime_root), not where source files happened to live.
    fs::write(
        source_root.join("Cargo.toml"),
        "[package]\nname='irrelevant-source-root'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("write source Cargo.toml");

    let app_source = source_root.join("main.rs");
    fs::write(
        &app_source,
        "use cache_manager::CacheRoot;\n\
         use std::fs;\n\
         use std::io;\n\
         fn main() -> io::Result<()> {\n\
             let root = CacheRoot::from_discovery()?;\n\
             let group = root.group(\"e2e\");\n\
             let ensured = group.ensure_dir()?;\n\
             let probe = group.touch(\"probe.txt\")?;\n\
             fs::metadata(&probe)?;\n\
             println!(\"{}\", root.path().display());\n\
             println!(\"{}\", ensured.display());\n\
             println!(\"{}\", probe.display());\n\
             Ok(())\n\
         }\n",
    )
    .expect("write tiny app source");

    let current_test_exe = env::current_exe().expect("current exe");
    let deps_dir = current_test_exe.parent().expect("deps dir");
    let cache_manager_rlib = latest_cache_manager_rlib(deps_dir);

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let binary_name = if std::env::consts::EXE_EXTENSION.is_empty() {
        "tiny-app".to_string()
    } else {
        format!("tiny-app.{}", std::env::consts::EXE_EXTENSION)
    };
    let output_binary = runtime_root.join(binary_name);

    let compile_output = Command::new(&rustc)
        .arg("--edition=2024")
        .arg(&app_source)
        .arg("-o")
        .arg(&output_binary)
        .arg("--extern")
        .arg(format!("cache_manager={}", cache_manager_rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps_dir.display()))
        .output()
        .expect("spawn rustc");

    assert!(
        compile_output.status.success(),
        "rustc failed: {}",
        String::from_utf8_lossy(&compile_output.stderr)
    );

    let run_output = Command::new(&output_binary)
        .current_dir(&runtime_root)
        .output()
        .expect("run tiny binary directly");

    assert!(
        run_output.status.success(),
        "tiny binary failed: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );

    let expected_root = runtime_root
        .canonicalize()
        .expect("canonicalize runtime root")
        .join(".cache");
    let expected_group = expected_root.join("e2e");
    let expected_probe = expected_group.join("probe.txt");

    assert!(
        expected_root.is_dir(),
        "expected default cache root to exist"
    );
    assert!(expected_group.is_dir(), "expected cache group to exist");
    assert!(expected_probe.is_file(), "expected probe file to exist");

    let stdout = String::from_utf8(run_output.stdout).expect("stdout utf8");
    let mut lines = stdout.lines();
    let root_line = lines.next().expect("root stdout line");
    let group_line = lines.next().expect("group stdout line");
    let probe_line = lines.next().expect("probe stdout line");

    assert_eq!(Path::new(root_line), expected_root.as_path());
    assert_eq!(Path::new(group_line), expected_group.as_path());
    assert_eq!(Path::new(probe_line), expected_probe.as_path());

    OpenOptions::new()
        .append(true)
        .open(&expected_probe)
        .expect("probe path remains writable");
}

fn latest_cache_manager_rlib(deps_dir: &Path) -> PathBuf {
    let mut rlibs = fs::read_dir(deps_dir)
        .expect("read deps dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("libcache_manager-") && name.ends_with(".rlib"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    rlibs.sort_by_key(|path| {
        path.metadata()
            .and_then(|m| m.modified())
            .ok()
            .unwrap_or(UNIX_EPOCH)
    });

    rlibs.pop().expect("find libcache_manager rlib")
}
