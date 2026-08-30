#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use driftctl::workspace::{WorkspaceError, isolate_workspace};

static TEMPORARY_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let sequence = TEMPORARY_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "driftctl-workspace-isolation-{case}-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is UTF-8")
}

fn repository(case: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temporary_directory(case);
    let source = root.join("source");
    let candidates = root.join("candidates");
    fs::create_dir(&source).expect("create source directory");
    fs::create_dir(&candidates).expect("create candidate directory");
    git(&source, &["init", "--quiet"]);
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&source, &["config", "user.name", "Fixture"]);
    (root, source, candidates)
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable fixture");
    let mut permissions = fs::metadata(path)
        .expect("inspect executable fixture")
        .permissions();
    permissions.set_mode(0o751);
    fs::set_permissions(path, permissions).expect("make fixture executable");
}

#[test]
fn isolates_an_equal_read_only_snapshot_with_dirty_untracked_ignored_and_symlink_content() {
    let (root, source, candidates) = repository("complete-snapshot");
    fs::write(
        source.join(".gitignore"),
        "ignored.txt\n.driftctl/\n.codex/\nevals/hidden-graders/\n",
    )
    .expect("write ignore policy");
    fs::write(source.join("tracked.txt"), "committed\n").expect("write tracked fixture");
    write_executable(&source.join("bin.sh"), "#!/bin/sh\necho committed\n");
    fs::create_dir_all(source.join("links")).expect("create symlink directory");
    symlink("../tracked.txt", source.join("links/tracked-link")).expect("create safe symlink");
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "fixture"]);

    fs::write(source.join("tracked.txt"), "dirty working-tree bytes\n")
        .expect("dirty tracked fixture");
    write_executable(&source.join("bin.sh"), "#!/bin/sh\necho dirty\n");
    fs::write(source.join("untracked.txt"), "selected untracked bytes\n")
        .expect("write untracked fixture");
    write_executable(
        &source.join("untracked-tool.sh"),
        "#!/bin/sh\necho selected-untracked\n",
    );
    fs::write(source.join("ignored.txt"), "must remain private\n").expect("write ignored fixture");
    fs::create_dir_all(source.join(".driftctl")).expect("create Driftctl state");
    fs::write(source.join(".driftctl/state.json"), "state").expect("write state fixture");
    fs::create_dir_all(source.join(".codex")).expect("create provider configuration");
    fs::write(source.join(".codex/auth.json"), "credential").expect("write credential fixture");
    fs::create_dir_all(source.join("evals/hidden-graders")).expect("create hidden grader");
    fs::write(
        source.join("evals/hidden-graders/check.py"),
        "private grader",
    )
    .expect("write hidden grader fixture");

    let source_status = git(&source, &["status", "--porcelain=v1"]);
    let pair = isolate_workspace(&source, &candidates).expect("isolate workspace snapshot");

    assert_ne!(pair.baseline().root(), pair.workflow().root());
    assert_eq!(
        pair.source_pre_manifest().head(),
        git(&source, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(pair.source_pre_manifest(), pair.source_post_manifest());
    assert_eq!(pair.source_pre_manifest(), pair.baseline().manifest());
    assert_eq!(pair.baseline().manifest(), pair.workflow().manifest());
    assert!(pair.baseline_diff().is_empty());
    assert!(pair.workflow_diff().is_empty());
    assert_eq!(source_status, git(&source, &["status", "--porcelain=v1"]));
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).expect("read source dirty file"),
        "dirty working-tree bytes\n"
    );

    for candidate in [pair.baseline(), pair.workflow()] {
        let root = candidate.root();
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).expect("read candidate tracked file"),
            "dirty working-tree bytes\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("untracked.txt")).expect("read candidate untracked file"),
            "selected untracked bytes\n"
        );
        assert_eq!(
            fs::metadata(root.join("bin.sh"))
                .expect("inspect candidate executable")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        assert_eq!(
            fs::metadata(root.join("untracked-tool.sh"))
                .expect("inspect candidate untracked executable")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        assert_eq!(
            fs::read_link(root.join("links/tracked-link")).expect("read candidate symlink"),
            PathBuf::from("../tracked.txt")
        );
        assert!(!root.join("ignored.txt").exists());
        assert!(!root.join(".driftctl").exists());
        assert!(!root.join(".codex").exists());
        assert!(!root.join("evals/hidden-graders").exists());
    }

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn refuses_relative_symlink_traversal_outside_the_source_root() {
    let (root, source, candidates) = repository("unsafe-symlink");
    fs::write(source.join("tracked.txt"), "tracked\n").expect("write tracked fixture");
    git(&source, &["add", "tracked.txt"]);
    git(&source, &["commit", "--quiet", "-m", "fixture"]);
    fs::write(root.join("outside.txt"), "outside\n").expect("write outside fixture");
    symlink("../outside.txt", source.join("escape")).expect("create escaping symlink");

    assert!(matches!(
        isolate_workspace(&source, &candidates),
        Err(WorkspaceError::UnsafeSymlink { .. })
    ));
    assert!(
        !candidates
            .read_dir()
            .expect("read candidates")
            .next()
            .is_some()
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn refuses_special_files_and_candidate_roots_inside_the_source() {
    let (root, source, candidates) = repository("special-file");
    fs::write(source.join("tracked.txt"), "tracked\n").expect("write tracked fixture");
    git(&source, &["add", "tracked.txt"]);
    git(&source, &["commit", "--quiet", "-m", "fixture"]);
    let fifo = source.join("untracked-fifo");
    let output = Command::new("mkfifo")
        .arg(&fifo)
        .output()
        .expect("run mkfifo");
    assert!(output.status.success(), "mkfifo failed: {output:?}");

    assert!(matches!(
        isolate_workspace(&source, &candidates),
        Err(WorkspaceError::UnsupportedFileType { .. })
    ));
    fs::remove_file(&fifo).expect("remove FIFO before source-root test");
    assert!(matches!(
        isolate_workspace(&source, &source),
        Err(WorkspaceError::CandidateRootInsideSource { .. })
    ));

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
