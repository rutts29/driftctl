//! Read-only source workspace snapshots for paired continuation candidates.
//!
//! This module deliberately owns the filesystem boundary. It reads a Git
//! worktree once into memory, writes only newly-created candidate roots, and
//! re-reads the source before returning. Candidate roots are retained for the
//! caller to inspect or clean up; this module never deletes a source path.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

static CANDIDATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A deterministic digest of the selected source workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceManifest {
    head: String,
    entries: Vec<ManifestEntry>,
    digest: String,
}

impl WorkspaceManifest {
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// One selected file or symlink in a [`WorkspaceManifest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    relative_path: String,
    mode: u32,
    content_digest: String,
    kind: ManifestEntryKind,
}

impl ManifestEntry {
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    #[must_use]
    pub fn mode(&self) -> u32 {
        self.mode
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }

    #[must_use]
    pub fn kind(&self) -> ManifestEntryKind {
        self.kind
    }
}

/// The filesystem kind represented by a manifest entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestEntryKind {
    File,
    Symlink,
}

/// One disposable candidate root and its verified manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateWorkspace {
    root: PathBuf,
    manifest: WorkspaceManifest,
}

impl CandidateWorkspace {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manifest(&self) -> &WorkspaceManifest {
        &self.manifest
    }
}

/// Differences observed between a candidate and the captured source manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestDiff {
    changed_paths: Vec<String>,
}

impl ManifestDiff {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed_paths.is_empty()
    }

    #[must_use]
    pub fn changed_paths(&self) -> &[String] {
        &self.changed_paths
    }
}

/// The checked source manifests and two independently materialized candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePair {
    source_pre_manifest: WorkspaceManifest,
    source_post_manifest: WorkspaceManifest,
    baseline: CandidateWorkspace,
    workflow: CandidateWorkspace,
    baseline_diff: ManifestDiff,
    workflow_diff: ManifestDiff,
}

impl WorkspacePair {
    #[must_use]
    pub fn source_pre_manifest(&self) -> &WorkspaceManifest {
        &self.source_pre_manifest
    }

    #[must_use]
    pub fn source_post_manifest(&self) -> &WorkspaceManifest {
        &self.source_post_manifest
    }

    #[must_use]
    pub fn baseline(&self) -> &CandidateWorkspace {
        &self.baseline
    }

    #[must_use]
    pub fn workflow(&self) -> &CandidateWorkspace {
        &self.workflow
    }

    #[must_use]
    pub fn baseline_diff(&self) -> &ManifestDiff {
        &self.baseline_diff
    }

    #[must_use]
    pub fn workflow_diff(&self) -> &ManifestDiff {
        &self.workflow_diff
    }
}

/// Failures at the Git and filesystem isolation boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceError {
    Git {
        action: &'static str,
        message: String,
    },
    InvalidSourcePath {
        path: PathBuf,
    },
    CandidateRootInsideSource {
        source: PathBuf,
        candidate_parent: PathBuf,
    },
    UnsafeSymlink {
        path: PathBuf,
        target: PathBuf,
    },
    UnsupportedFileType {
        path: PathBuf,
    },
    SourceChanged,
    CandidateManifestMismatch {
        changed_paths: Vec<String>,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git { action, message } => write!(formatter, "{action}: {message}"),
            Self::InvalidSourcePath { path } => {
                write!(formatter, "refusing unsafe source path: {}", path.display())
            }
            Self::CandidateRootInsideSource {
                source,
                candidate_parent,
            } => write!(
                formatter,
                "candidate parent {} is inside source {}",
                candidate_parent.display(),
                source.display()
            ),
            Self::UnsafeSymlink { path, target } => write!(
                formatter,
                "refusing unsafe symlink {} -> {}",
                path.display(),
                target.display()
            ),
            Self::UnsupportedFileType { path } => {
                write!(formatter, "refusing special file: {}", path.display())
            }
            Self::SourceChanged => formatter.write_str("source workspace changed during snapshot"),
            Self::CandidateManifestMismatch { changed_paths } => write!(
                formatter,
                "candidate manifest differs from source: {}",
                changed_paths.join(", ")
            ),
            Self::Io {
                action,
                path,
                message,
            } => write!(formatter, "{action} {}: {message}", path.display()),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Capture a source worktree into two equal disposable roots.
///
/// The explicit inclusion policy is tracked files plus all non-ignored
/// untracked files. Ignored files are not copied. Git internals, Driftctl
/// state, common provider configuration/authentication locations, hidden
/// grader paths, and common secret-file names are excluded even when tracked.
pub fn isolate_workspace(
    source: impl AsRef<Path>,
    candidate_parent: impl AsRef<Path>,
) -> Result<WorkspacePair, WorkspaceError> {
    let snapshot = capture(source.as_ref())?;
    let candidate_parent =
        candidate_parent
            .as_ref()
            .canonicalize()
            .map_err(|error| WorkspaceError::Io {
                action: "canonicalize candidate parent",
                path: candidate_parent.as_ref().to_path_buf(),
                message: error.to_string(),
            })?;
    if candidate_parent.starts_with(&snapshot.source_root) {
        return Err(WorkspaceError::CandidateRootInsideSource {
            source: snapshot.source_root,
            candidate_parent,
        });
    }

    let baseline_root = create_candidate_root(&candidate_parent, "baseline")?;
    let workflow_root = create_candidate_root(&candidate_parent, "workflow")?;
    let baseline = materialize_candidate(&snapshot, baseline_root)?;
    let workflow = materialize_candidate(&snapshot, workflow_root)?;
    let baseline_diff = manifest_diff(&snapshot.manifest, &baseline.manifest);
    let workflow_diff = manifest_diff(&snapshot.manifest, &workflow.manifest);
    if !baseline_diff.is_empty() {
        return Err(WorkspaceError::CandidateManifestMismatch {
            changed_paths: baseline_diff.changed_paths,
        });
    }
    if !workflow_diff.is_empty() {
        return Err(WorkspaceError::CandidateManifestMismatch {
            changed_paths: workflow_diff.changed_paths,
        });
    }

    let source_post = capture(&snapshot.source_root)?;
    if source_post.manifest != snapshot.manifest {
        return Err(WorkspaceError::SourceChanged);
    }

    Ok(WorkspacePair {
        source_pre_manifest: snapshot.manifest,
        source_post_manifest: source_post.manifest,
        baseline,
        workflow,
        baseline_diff,
        workflow_diff,
    })
}

struct CapturedWorkspace {
    source_root: PathBuf,
    manifest: WorkspaceManifest,
    entries: Vec<CapturedEntry>,
}

#[derive(Clone)]
struct CapturedEntry {
    relative_path: PathBuf,
    manifest: ManifestEntry,
    contents: CapturedContents,
}

#[derive(Clone)]
enum CapturedContents {
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn capture(source: &Path) -> Result<CapturedWorkspace, WorkspaceError> {
    let source_root = source.canonicalize().map_err(|error| WorkspaceError::Io {
        action: "canonicalize source root",
        path: source.to_path_buf(),
        message: error.to_string(),
    })?;
    let metadata = fs::metadata(&source_root)
        .map_err(|error| io_error("inspect source root", &source_root, error))?;
    if !metadata.is_dir() {
        return Err(WorkspaceError::InvalidSourcePath { path: source_root });
    }
    refuse_special_files(&source_root, Path::new(""))?;

    let head = git_text(&source_root, &["rev-parse", "--verify", "HEAD"])?;
    let tracked = git_paths(&source_root, &["ls-files", "-z"])?;
    let untracked = untracked_paths(&source_root)?;
    let paths = tracked
        .into_iter()
        .chain(untracked)
        .filter(|path| !is_excluded(path))
        .collect::<BTreeSet<_>>();

    let mut entries = Vec::new();
    for relative_path in paths {
        // A deleted tracked file is intentionally absent from the working-tree
        // snapshot. Its HEAD still anchors the captured checkpoint.
        if let Some(entry) = capture_entry(&source_root, &relative_path)? {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| {
        left.manifest
            .relative_path
            .cmp(&right.manifest.relative_path)
    });
    let manifest = build_manifest(
        head,
        entries.iter().map(|entry| entry.manifest.clone()).collect(),
    );
    Ok(CapturedWorkspace {
        source_root,
        manifest,
        entries,
    })
}

fn materialize_candidate(
    snapshot: &CapturedWorkspace,
    root: PathBuf,
) -> Result<CandidateWorkspace, WorkspaceError> {
    for entry in &snapshot.entries {
        let destination = root.join(&entry.relative_path);
        let parent = destination
            .parent()
            .ok_or_else(|| WorkspaceError::InvalidSourcePath {
                path: entry.relative_path.clone(),
            })?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create candidate directory", parent, error))?;
        match &entry.contents {
            CapturedContents::File(bytes) => {
                let mut file = File::options()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|error| io_error("create candidate file", &destination, error))?;
                file.write_all(bytes)
                    .map_err(|error| io_error("write candidate file", &destination, error))?;
                file.sync_all()
                    .map_err(|error| io_error("sync candidate file", &destination, error))?;
                let mut permissions = file
                    .metadata()
                    .map_err(|error| io_error("inspect candidate file", &destination, error))?
                    .permissions();
                permissions.set_mode(entry.manifest.mode);
                fs::set_permissions(&destination, permissions)
                    .map_err(|error| io_error("set candidate file mode", &destination, error))?;
            }
            CapturedContents::Symlink(target) => symlink(target, &destination)
                .map_err(|error| io_error("create candidate symlink", &destination, error))?,
        }
    }
    let entries = snapshot
        .entries
        .iter()
        .map(|entry| capture_entry(&root, &entry.relative_path))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let manifest = build_manifest(
        snapshot.manifest.head.clone(),
        entries.into_iter().map(|entry| entry.manifest).collect(),
    );
    Ok(CandidateWorkspace { root, manifest })
}

fn capture_entry(
    root: &Path,
    relative_path: &Path,
) -> Result<Option<CapturedEntry>, WorkspaceError> {
    validate_relative_path(relative_path)?;
    let path = checked_path(root, relative_path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect source entry", &path, error)),
    };
    let mode = metadata.permissions().mode() & 0o7777;
    let relative_path_text = relative_path
        .to_str()
        .ok_or_else(|| WorkspaceError::InvalidSourcePath {
            path: relative_path.to_path_buf(),
        })?
        .to_owned();
    if metadata.is_file() {
        let bytes = fs::read(&path).map_err(|error| io_error("read source file", &path, error))?;
        let manifest = ManifestEntry {
            relative_path: relative_path_text,
            mode,
            content_digest: sha256_digest(&bytes),
            kind: ManifestEntryKind::File,
        };
        Ok(Some(CapturedEntry {
            relative_path: relative_path.to_path_buf(),
            manifest,
            contents: CapturedContents::File(bytes),
        }))
    } else if metadata.file_type().is_symlink() {
        let target =
            fs::read_link(&path).map_err(|error| io_error("read source symlink", &path, error))?;
        validate_safe_symlink(root, relative_path, &target)?;
        let target_text = target
            .to_str()
            .ok_or_else(|| WorkspaceError::UnsafeSymlink {
                path: path.clone(),
                target: target.clone(),
            })?;
        let manifest = ManifestEntry {
            relative_path: relative_path_text,
            mode,
            content_digest: sha256_digest(target_text.as_bytes()),
            kind: ManifestEntryKind::Symlink,
        };
        Ok(Some(CapturedEntry {
            relative_path: relative_path.to_path_buf(),
            manifest,
            contents: CapturedContents::Symlink(target),
        }))
    } else {
        Err(WorkspaceError::UnsupportedFileType { path })
    }
}

fn checked_path(root: &Path, relative_path: &Path) -> Result<PathBuf, WorkspaceError> {
    let mut current = root.to_path_buf();
    let components = relative_path.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(component) = component else {
            return Err(WorkspaceError::InvalidSourcePath {
                path: relative_path.to_path_buf(),
            });
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| io_error("inspect source path component", &current, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkspaceError::InvalidSourcePath { path: current });
        }
    }
    Ok(root.join(relative_path))
}

fn refuse_special_files(root: &Path, relative_directory: &Path) -> Result<(), WorkspaceError> {
    let directory = root.join(relative_directory);
    for entry in fs::read_dir(&directory)
        .map_err(|error| io_error("read source directory", &directory, error))?
    {
        let entry =
            entry.map_err(|error| io_error("read source directory entry", &directory, error))?;
        let relative_path = relative_directory.join(entry.file_name());
        if is_excluded(&relative_path) {
            continue;
        }
        let path = root.join(&relative_path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("inspect source entry", &path, error))?;
        if metadata.is_dir() {
            refuse_special_files(root, &relative_path)?;
        } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err(WorkspaceError::UnsupportedFileType { path });
        }
    }
    Ok(())
}

fn validate_safe_symlink(
    root: &Path,
    relative_path: &Path,
    target: &Path,
) -> Result<(), WorkspaceError> {
    let full_path = root.join(relative_path);
    if target.is_absolute() {
        return Err(WorkspaceError::UnsafeSymlink {
            path: full_path,
            target: target.to_path_buf(),
        });
    }
    let resolved = full_path
        .parent()
        .unwrap_or(root)
        .join(target)
        .canonicalize()
        .map_err(|_| WorkspaceError::UnsafeSymlink {
            path: full_path.clone(),
            target: target.to_path_buf(),
        })?;
    if resolved.starts_with(root) {
        Ok(())
    } else {
        Err(WorkspaceError::UnsafeSymlink {
            path: full_path,
            target: target.to_path_buf(),
        })
    }
}

fn create_candidate_root(parent: &Path, arm: &str) -> Result<PathBuf, WorkspaceError> {
    for _ in 0..1024 {
        let sequence = CANDIDATE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = parent.join(format!("driftctl-{arm}-{}-{sequence}", std::process::id()));
        match fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create candidate root", &root, error)),
        }
    }
    Err(WorkspaceError::Io {
        action: "create candidate root",
        path: parent.to_path_buf(),
        message: "could not allocate a unique candidate root".to_owned(),
    })
}

fn manifest_diff(expected: &WorkspaceManifest, actual: &WorkspaceManifest) -> ManifestDiff {
    if expected == actual {
        return ManifestDiff {
            changed_paths: Vec::new(),
        };
    }
    let mut paths = expected
        .entries
        .iter()
        .map(|entry| entry.relative_path.clone())
        .chain(
            actual
                .entries
                .iter()
                .map(|entry| entry.relative_path.clone()),
        )
        .collect::<BTreeSet<_>>();
    paths.retain(|path| {
        expected
            .entries
            .iter()
            .find(|entry| &entry.relative_path == path)
            != actual
                .entries
                .iter()
                .find(|entry| &entry.relative_path == path)
    });
    if expected.head != actual.head {
        paths.insert("<HEAD>".to_owned());
    }
    ManifestDiff {
        changed_paths: paths.into_iter().collect(),
    }
}

fn build_manifest(head: String, entries: Vec<ManifestEntry>) -> WorkspaceManifest {
    let mut hasher = Sha256::new();
    update_hash(&mut hasher, head.as_bytes());
    for entry in &entries {
        update_hash(&mut hasher, entry.relative_path.as_bytes());
        hasher.update(entry.mode.to_be_bytes());
        hasher.update([match entry.kind {
            ManifestEntryKind::File => 0,
            ManifestEntryKind::Symlink => 1,
        }]);
        update_hash(&mut hasher, entry.content_digest.as_bytes());
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    WorkspaceManifest {
        head,
        entries,
        digest,
    }
}

fn update_hash(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(
        u64::try_from(bytes.len())
            .expect("platform length fits u64")
            .to_be_bytes(),
    );
    hasher.update(bytes);
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn git_text(root: &Path, arguments: &[&str]) -> Result<String, WorkspaceError> {
    let output = git_output(root, arguments, "read Git HEAD")?;
    String::from_utf8(output)
        .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|error| WorkspaceError::Git {
            action: "read Git output",
            message: error.to_string(),
        })
}

fn git_paths(root: &Path, arguments: &[&str]) -> Result<Vec<PathBuf>, WorkspaceError> {
    let output = git_output(root, arguments, "list Git paths")?;
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path =
                std::str::from_utf8(path).map_err(|_| WorkspaceError::InvalidSourcePath {
                    path: PathBuf::from("<non-UTF-8 Git path>"),
                })?;
            let path = PathBuf::from(path);
            validate_relative_path(&path)?;
            Ok(path)
        })
        .collect()
}

fn untracked_paths(root: &Path) -> Result<Vec<PathBuf>, WorkspaceError> {
    let output = git_output(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "list untracked Git paths",
    )?;
    output
        .split(|byte| *byte == 0)
        .filter_map(|record| record.strip_prefix(b"?? "))
        .map(|path| {
            let path =
                std::str::from_utf8(path).map_err(|_| WorkspaceError::InvalidSourcePath {
                    path: PathBuf::from("<non-UTF-8 Git path>"),
                })?;
            let path = PathBuf::from(path);
            validate_relative_path(&path)?;
            Ok(path)
        })
        .collect()
}

fn git_output(
    root: &Path,
    arguments: &[&str],
    action: &'static str,
) -> Result<Vec<u8>, WorkspaceError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|error| WorkspaceError::Git {
            action,
            message: error.to_string(),
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(WorkspaceError::Git {
            action,
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn validate_relative_path(path: &Path) -> Result<(), WorkspaceError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::InvalidSourcePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn is_excluded(path: &Path) -> bool {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return true;
    };
    let first = first.to_string_lossy();
    if matches!(
        first.as_ref(),
        ".git" | ".driftctl" | ".codex" | ".claude" | ".cursor" | ".aider"
    ) {
        return true;
    }
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    if file_name == ".env"
        || file_name.starts_with(".env.")
        || matches!(
            file_name.as_ref(),
            "id_rsa" | "id_ed25519" | "credentials.json"
        )
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
    {
        return true;
    }
    path.components()
        .filter_map(|component| match component {
            Component::Normal(component) => Some(component.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .any(|component| {
            component == ".hidden" || (component.contains("hidden") && component.contains("grader"))
        })
}

fn io_error(action: &'static str, path: &Path, error: std::io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        action,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
