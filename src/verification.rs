//! Requirement-specific external command verification.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct VerificationRequest {
    candidate: PathBuf,
    requirement_id: String,
    command: Vec<OsString>,
    artifact_root: PathBuf,
}

impl VerificationRequest {
    pub fn new(
        candidate: impl AsRef<Path>,
        requirement_id: impl Into<String>,
        command: impl IntoIterator<Item = OsString>,
        artifact_root: impl AsRef<Path>,
    ) -> Result<Self, VerificationError> {
        let candidate = candidate
            .as_ref()
            .canonicalize()
            .map_err(|error| VerificationError::io("canonicalize candidate", error))?;
        if !candidate.is_dir() {
            return Err(VerificationError::invalid("candidate must be a directory"));
        }
        let requirement_id = requirement_id.into();
        if requirement_id.trim().is_empty()
            || requirement_id.len() > 512
            || requirement_id.chars().any(char::is_control)
        {
            return Err(VerificationError::invalid("invalid requirement ID"));
        }
        let command = command.into_iter().collect::<Vec<_>>();
        if command.is_empty() || command.iter().any(|argument| argument.is_empty()) {
            return Err(VerificationError::invalid(
                "verification command must not be empty",
            ));
        }
        Ok(Self {
            candidate,
            requirement_id,
            command,
            artifact_root: artifact_root.as_ref().to_path_buf(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    ProtectedInputChanged,
    CandidateChanged,
}

impl VerificationStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::ProtectedInputChanged => "protected_input_changed",
            Self::CandidateChanged => "candidate_changed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationResult {
    pub schema_version: u32,
    pub requirement_id: String,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub command_digest: String,
    pub verifier_digest: String,
    pub candidate_before_digest: String,
    pub candidate_after_digest: String,
    pub stdout_digest: String,
    pub stderr_digest: String,
    pub artifact_id: String,
    pub started_at_unix_ms: u128,
    pub elapsed_ms: u128,
}

impl VerificationResult {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.status == VerificationStatus::Passed
    }
}

pub fn verify(request: &VerificationRequest) -> Result<VerificationResult, VerificationError> {
    let verifier_before = verifier_digest(&request.candidate, &request.command)?;
    let candidate_before = candidate_digest(&request.candidate)?;
    ensure_private_directory(&request.artifact_root)?;
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| VerificationError::invalid("system clock is before Unix epoch"))?
        .as_millis();
    let started = Instant::now();
    let output = Command::new(&request.command[0])
        .args(&request.command[1..])
        .current_dir(&request.candidate)
        .output()
        .map_err(|error| VerificationError::io("execute verifier", error))?;
    let elapsed_ms = started.elapsed().as_millis();
    let verifier_after = verifier_digest(&request.candidate, &request.command)?;
    let candidate_after = candidate_digest(&request.candidate)?;
    let stdout_digest = digest_bytes(&output.stdout);
    let stderr_digest = digest_bytes(&output.stderr);
    let command_digest = command_digest(&request.command);
    let artifact_id = format!(
        "verification-{}-{}-{}",
        std::process::id(),
        started_at_unix_ms,
        ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    write_artifacts(
        &request.artifact_root.join(&artifact_id),
        &request.command,
        &output.stdout,
        &output.stderr,
    )?;
    let status = if verifier_before != verifier_after {
        VerificationStatus::ProtectedInputChanged
    } else if candidate_before != candidate_after {
        VerificationStatus::CandidateChanged
    } else if output.status.success() {
        VerificationStatus::Passed
    } else {
        VerificationStatus::Failed
    };
    Ok(VerificationResult {
        schema_version: 1,
        requirement_id: request.requirement_id.clone(),
        status,
        exit_code: output.status.code(),
        command_digest,
        verifier_digest: verifier_before,
        candidate_before_digest: candidate_before,
        candidate_after_digest: candidate_after,
        stdout_digest,
        stderr_digest,
        artifact_id,
        started_at_unix_ms,
        elapsed_ms,
    })
}

#[derive(Debug)]
pub struct VerificationError(String);

impl VerificationError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn io(action: &str, error: std::io::Error) -> Self {
        Self(format!("{action}: {error}"))
    }
}

impl fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for VerificationError {}

fn verifier_digest(candidate: &Path, command: &[OsString]) -> Result<String, VerificationError> {
    let mut hasher = Sha256::new();
    hasher.update(b"driftctl.verifier.v1\0");
    for argument in command {
        update_os_hash(&mut hasher, argument);
        let path = Path::new(argument);
        if !path.is_absolute() || !path.exists() {
            continue;
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| VerificationError::io("canonicalize verifier input", error))?;
        if canonical.starts_with(candidate) {
            return Err(VerificationError::invalid(
                "verifier inputs must remain outside the candidate",
            ));
        }
        let metadata = fs::symlink_metadata(&canonical)
            .map_err(|error| VerificationError::io("inspect verifier input", error))?;
        if metadata.file_type().is_file() {
            update_hash(
                &mut hasher,
                &fs::read(&canonical)
                    .map_err(|error| VerificationError::io("read verifier input", error))?,
            );
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn candidate_digest(candidate: &Path) -> Result<String, VerificationError> {
    let output = Command::new("git")
        .current_dir(candidate)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|error| VerificationError::io("list candidate files", error))?;
    if !output.status.success() {
        return Err(VerificationError::invalid(
            "candidate is not a readable Git checkpoint",
        ));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(PathBuf::from)
                .map_err(|_| VerificationError::invalid("candidate path is not UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    let mut hasher = Sha256::new();
    hasher.update(b"driftctl.candidate.v1\0");
    for relative in paths {
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(VerificationError::invalid("unsafe candidate path"));
        }
        update_os_hash(&mut hasher, relative.as_os_str());
        let path = candidate.join(&relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| VerificationError::io("inspect candidate file", error))?;
        if metadata.file_type().is_symlink() {
            update_os_hash(
                &mut hasher,
                fs::read_link(&path)
                    .map_err(|error| VerificationError::io("read candidate symlink", error))?
                    .as_os_str(),
            );
        } else if metadata.file_type().is_file() {
            update_hash(
                &mut hasher,
                &fs::read(&path)
                    .map_err(|error| VerificationError::io("read candidate file", error))?,
            );
        } else {
            return Err(VerificationError::invalid(
                "candidate contains an unsupported file type",
            ));
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn command_digest(command: &[OsString]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"driftctl.command.v1\0");
    for argument in command {
        update_os_hash(&mut hasher, argument);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn update_hash(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(bytes.len().to_be_bytes());
    hasher.update(bytes);
}

#[cfg(unix)]
fn update_os_hash(hasher: &mut Sha256, value: &OsStr) {
    use std::os::unix::ffi::OsStrExt as _;
    update_hash(hasher, value.as_bytes());
}

#[cfg(not(unix))]
fn update_os_hash(hasher: &mut Sha256, value: &OsStr) {
    update_hash(hasher, value.to_string_lossy().as_bytes());
}

fn ensure_private_directory(path: &Path) -> Result<(), VerificationError> {
    crate::run_store::ensure_private_directory(path)
        .map_err(|error| VerificationError::invalid(error.to_string()))
}

fn write_artifacts(
    directory: &Path,
    command: &[OsString],
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), VerificationError> {
    fs::create_dir(directory)
        .map_err(|error| VerificationError::io("create verification artifact", error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| VerificationError::io("protect verification artifact", error))?;
    }
    let command = command
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let metadata = serde_json::to_vec(&serde_json::json!({"schema_version":1,"command":command}))
        .map_err(|_| VerificationError::invalid("could not encode verifier metadata"))?;
    write_private_file(&directory.join("command.json"), &metadata)?;
    write_private_file(&directory.join("stdout.bin"), stdout)?;
    write_private_file(&directory.join("stderr.bin"), stderr)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), VerificationError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| VerificationError::io("create verification artifact file", error))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| VerificationError::io("write verification artifact file", error))?;
    File::open(path.parent().expect("artifact file has parent"))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| VerificationError::io("sync verification artifact directory", error))
}
