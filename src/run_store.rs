//! Local, durable storage for one v2 intent run.
//!
//! This module is intentionally below the CLI boundary.  It owns the XDG
//! state layout and its crash-safe projection/history commit protocol, while
//! [`crate::intent_history::History`] and [`crate::projection::ActiveProjection`]
//! remain the authorities for event and projection validity.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::intent_history::{EventRecord, History, HistoryError};
use crate::projection::{ActiveProjection, ProjectionError, project};

const APPLICATION_DIRECTORY: &str = "driftctl";
const REPOSITORIES_DIRECTORY: &str = "repositories";
const RUNS_DIRECTORY: &str = "runs";
const HISTORY_DIRECTORY: &str = "history";
const PROJECTION_FILE: &str = "projection.json";
const SOURCE_FILE: &str = "source.json";
const CANDIDATE_FILE: &str = "candidate.json";
const COMPLETION_GATES_FILE: &str = "completion-gates.json";
const PENDING_FILE: &str = "pending.jsonl";
const LOCK_FILE: &str = ".writer.lock";
const TEMP_PROJECTION_PREFIX: &str = ".projection.json.tmp-";
const TEMP_SOURCE_PREFIX: &str = ".source.json.tmp-";
const TEMP_CANDIDATE_PREFIX: &str = ".candidate.json.tmp-";
const TEMP_COMPLETION_GATES_PREFIX: &str = ".completion-gates.json.tmp-";
const MAX_SOURCE_CURSOR_IDENTIFIER_BYTES: usize = 16 * 1024;

/// Exact schema accepted for the private source cursor state.
pub const SOURCE_CURSOR_SCHEMA_VERSION: u32 = 1;
pub const CANDIDATE_BINDING_SCHEMA_VERSION: u32 = 1;
pub const COMPLETION_GATE_STATE_SCHEMA_VERSION: u32 = 1;

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An opaque run identifier accepted as a single state-directory component.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Parse an opaque run identifier without accepting path syntax.
    pub fn parse(value: impl Into<String>) -> Result<Self, RunStoreError> {
        let value = value.into();
        let valid_length = (1..=64).contains(&value.len());
        let valid_first = value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric);
        let valid_characters = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if valid_length && valid_first && valid_characters {
            Ok(Self(value))
        } else {
            Err(RunStoreError::InvalidRunId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RunId {
    type Error = RunStoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for RunId {
    type Error = RunStoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl AsRef<str> for RunId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One accepted source position, retaining identity and digest but never the
/// provider transcript content itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecordDigest {
    id: String,
    content_digest: String,
    #[serde(default = "default_source_record_role")]
    role: crate::intent_history::SourceRole,
}

impl SourceRecordDigest {
    #[must_use]
    pub fn new(id: impl Into<String>, content_digest: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content_digest: content_digest.into(),
            role: crate::intent_history::SourceRole::User,
        }
    }

    #[must_use]
    pub fn with_role(
        id: impl Into<String>,
        content_digest: impl Into<String>,
        role: crate::intent_history::SourceRole,
    ) -> Self {
        Self {
            id: id.into(),
            content_digest: content_digest.into(),
            role,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }

    #[must_use]
    pub fn role(&self) -> &crate::intent_history::SourceRole {
        &self.role
    }
}

fn default_source_record_role() -> crate::intent_history::SourceRole {
    crate::intent_history::SourceRole::User
}

/// Private, digest-only cursor for the source records accepted by a run.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCursor {
    schema_version: u32,
    provider: crate::intent_history::SourceProvider,
    session_locator: String,
    repository_digest: String,
    accepted_records: Vec<SourceRecordDigest>,
    head: String,
    digest: String,
}

impl fmt::Debug for SourceCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceCursor")
            .field("schema_version", &self.schema_version)
            .field("provider", &self.provider)
            .field("repository_digest", &self.repository_digest)
            .field("accepted_records", &self.accepted_records)
            .field("head", &self.head)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl SourceCursor {
    pub fn new(
        provider: crate::intent_history::SourceProvider,
        session_locator: impl Into<String>,
        repository_digest: impl Into<String>,
        accepted_records: Vec<SourceRecordDigest>,
    ) -> Result<Self, SourceCursorError> {
        let cursor = Self {
            schema_version: SOURCE_CURSOR_SCHEMA_VERSION,
            provider,
            session_locator: session_locator.into(),
            repository_digest: repository_digest.into(),
            head: accepted_records
                .last()
                .map(|record| record.id.clone())
                .unwrap_or_default(),
            digest: source_cursor_digest(&accepted_records),
            accepted_records,
        };
        cursor.validate()?;
        Ok(cursor)
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn provider(&self) -> &crate::intent_history::SourceProvider {
        &self.provider
    }

    /// The provider locator is private state and intentionally omitted from
    /// [`fmt::Debug`] output.
    #[must_use]
    pub fn session_locator_private(&self) -> &str {
        &self.session_locator
    }

    #[must_use]
    pub fn repository_digest(&self) -> &str {
        &self.repository_digest
    }

    #[must_use]
    pub fn accepted_records(&self) -> &[SourceRecordDigest] {
        &self.accepted_records
    }

    #[must_use]
    pub fn accepted_record_count(&self) -> usize {
        self.accepted_records.len()
    }

    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn compare(&self, observed: &Self) -> Result<SourceCursorComparison, SourceCursorError> {
        self.validate()?;
        observed.validate()?;
        if self.provider != observed.provider
            || self.session_locator != observed.session_locator
            || self.repository_digest != observed.repository_digest
        {
            return Ok(SourceCursorComparison::SessionMismatch);
        }

        for (index, accepted) in self.accepted_records.iter().enumerate() {
            let Some(current) = observed.accepted_records.get(index) else {
                return Ok(SourceCursorComparison::Stale {
                    accepted_head: self.head.clone(),
                    observed_head: observed.head.clone(),
                });
            };
            if accepted != current {
                return Ok(SourceCursorComparison::Rewrite {
                    accepted_record_id: accepted.id.clone(),
                    observed_record_id: current.id.clone(),
                });
            }
        }

        let new_records = observed.accepted_records[self.accepted_records.len()..].to_vec();
        if new_records.is_empty() {
            Ok(SourceCursorComparison::Current)
        } else {
            Ok(SourceCursorComparison::NewRecords(new_records))
        }
    }

    pub fn validate(&self) -> Result<(), SourceCursorError> {
        if self.schema_version != SOURCE_CURSOR_SCHEMA_VERSION {
            return Err(SourceCursorError::UnsupportedSchemaVersion {
                expected: SOURCE_CURSOR_SCHEMA_VERSION,
                found: self.schema_version,
            });
        }
        validate_source_identifier(&self.session_locator, "source session locator")?;
        validate_source_digest(&self.repository_digest, "source repository digest")?;
        if self.accepted_records.is_empty() {
            return Err(SourceCursorError::EmptyRecords);
        }
        let mut ids = BTreeSet::new();
        for record in &self.accepted_records {
            validate_source_identifier(&record.id, "source record id")?;
            validate_source_digest(&record.content_digest, "source record content digest")?;
            if !ids.insert(&record.id) {
                return Err(SourceCursorError::DuplicateRecordId(record.id.clone()));
            }
        }
        if self.head
            != self
                .accepted_records
                .last()
                .expect("checked nonempty records")
                .id
        {
            return Err(SourceCursorError::SourceHeadMismatch);
        }
        if self.digest != source_cursor_digest(&self.accepted_records) {
            return Err(SourceCursorError::SourceDigestMismatch);
        }
        Ok(())
    }
}

/// The exact-prefix relationship between accepted and currently observed
/// source records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceCursorComparison {
    Current,
    NewRecords(Vec<SourceRecordDigest>),
    Stale {
        accepted_head: String,
        observed_head: String,
    },
    Rewrite {
        accepted_record_id: String,
        observed_record_id: String,
    },
    SessionMismatch,
}

/// Invalid private source cursor state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceCursorError {
    UnsupportedSchemaVersion { expected: u32, found: u32 },
    InvalidField(&'static str),
    InvalidDigest(&'static str),
    EmptyRecords,
    DuplicateRecordId(String),
    SourceHeadMismatch,
    SourceDigestMismatch,
}

impl fmt::Display for SourceCursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { expected, found } => write!(
                formatter,
                "unsupported source cursor schema version: expected {expected}, found {found}"
            ),
            Self::InvalidField(field) => write!(formatter, "invalid source cursor field: {field}"),
            Self::InvalidDigest(field) => {
                write!(formatter, "invalid source cursor digest: {field}")
            }
            Self::EmptyRecords => formatter.write_str("source cursor has no accepted records"),
            Self::DuplicateRecordId(id) => write!(formatter, "duplicate source record id: {id}"),
            Self::SourceHeadMismatch => {
                formatter.write_str("source cursor head does not match accepted records")
            }
            Self::SourceDigestMismatch => {
                formatter.write_str("source cursor digest does not match accepted records")
            }
        }
    }
}

impl std::error::Error for SourceCursorError {}

/// Recoverable failures at the local filesystem boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunStoreError {
    InvalidRunId,
    Busy,
    RunAlreadyExists,
    RunNotFound,
    SymlinkRefused {
        path: PathBuf,
    },
    InvalidStateComponent {
        path: PathBuf,
    },
    ImmutableHistorySegment {
        path: PathBuf,
    },
    PrivateStatePermissions {
        path: PathBuf,
    },
    ReviewAlreadyRecorded,
    ProjectionMismatch,
    Io {
        action: &'static str,
        path: PathBuf,
        message: String,
    },
    History(HistoryError),
    Projection(ProjectionError),
    SourceCursor(SourceCursorError),
    Serialization(String),
}

impl fmt::Display for RunStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunId => formatter.write_str("invalid opaque run id"),
            Self::Busy => formatter.write_str("run store is busy"),
            Self::RunAlreadyExists => formatter.write_str("run store already exists"),
            Self::RunNotFound => formatter.write_str("run store does not exist"),
            Self::SymlinkRefused { path } => {
                write!(
                    formatter,
                    "refusing symlinked state component: {}",
                    path.display()
                )
            }
            Self::InvalidStateComponent { path } => {
                write!(formatter, "invalid state component: {}", path.display())
            }
            Self::ImmutableHistorySegment { path } => {
                write!(
                    formatter,
                    "immutable history segment already exists: {}",
                    path.display()
                )
            }
            Self::PrivateStatePermissions { path } => {
                write!(
                    formatter,
                    "private state file has unsafe permissions: {}",
                    path.display()
                )
            }
            Self::ReviewAlreadyRecorded => formatter.write_str(
                "review is already recorded for this candidate checkpoint; change the candidate before reviewing again",
            ),
            Self::ProjectionMismatch => {
                formatter.write_str("stored projection does not match validated history")
            }
            Self::Io {
                action,
                path,
                message,
            } => write!(formatter, "{action} {}: {message}", path.display()),
            Self::History(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
            Self::SourceCursor(error) => error.fmt(formatter),
            Self::Serialization(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RunStoreError {}

impl From<HistoryError> for RunStoreError {
    fn from(error: HistoryError) -> Self {
        Self::History(error)
    }
}

impl From<ProjectionError> for RunStoreError {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}

impl From<SourceCursorError> for RunStoreError {
    fn from(error: SourceCursorError) -> Self {
        Self::SourceCursor(error)
    }
}

/// State reconstructed after reopening a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredRun {
    /// The last atomically committed active projection.
    pub projection: ActiveProjection,
    /// All events represented by immutable history plus the unrotated delta.
    pub history: History,
    /// Events still held in `pending.jsonl`.
    pub pending_delta: Vec<EventRecord>,
    /// The optional private source cursor.  Runs created before this state
    /// record existed remain readable without an implicit migration.
    pub source_cursor: Option<SourceCursor>,
}

/// Private identity of the one continued child accepted for run-bound checks.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateBinding {
    schema_version: u32,
    child_thread_id: String,
    candidate_path: PathBuf,
    approved_goal_digest: String,
}

impl fmt::Debug for CandidateBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CandidateBinding")
            .field("schema_version", &self.schema_version)
            .field("child_thread_id", &self.child_thread_id)
            .finish_non_exhaustive()
    }
}

impl CandidateBinding {
    #[must_use]
    pub fn child_thread_id(&self) -> &str {
        &self.child_thread_id
    }

    /// The candidate path is private local state and must not enter a public bundle.
    #[must_use]
    pub fn candidate_path_private(&self) -> &Path {
        &self.candidate_path
    }

    #[must_use]
    pub fn approved_goal_digest(&self) -> &str {
        &self.approved_goal_digest
    }

    #[must_use]
    pub fn matches_approved_goal(&self, objective: &str) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(b"driftctl.approved-child-goal.v1\0");
        hasher.update(objective.as_bytes());
        self.approved_goal_digest == format!("sha256:{:x}", hasher.finalize())
    }
}

/// Required run-level evidence gates beyond per-requirement checks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionGate {
    Regression,
    Integration,
    ProtectedScope,
    Review,
}

impl CompletionGate {
    pub const ALL: [Self; 4] = [
        Self::Regression,
        Self::Integration,
        Self::ProtectedScope,
        Self::Review,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regression => "regression",
            Self::Integration => "integration",
            Self::ProtectedScope => "protected_scope",
            Self::Review => "review",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionGateRecord {
    pub gate: CompletionGate,
    pub passed: bool,
    pub status: String,
    pub candidate_digest: String,
    pub artifact_id: String,
    pub command_digest: String,
    pub verifier_digest: String,
    pub stdout_digest: String,
    pub stderr_digest: String,
    pub started_at_unix_ms: u128,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionGateState {
    schema_version: u32,
    records: BTreeMap<CompletionGate, CompletionGateRecord>,
}

impl CompletionGateState {
    fn empty() -> Self {
        Self {
            schema_version: COMPLETION_GATE_STATE_SCHEMA_VERSION,
            records: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn record(&self, gate: CompletionGate) -> Option<&CompletionGateRecord> {
        self.records.get(&gate)
    }
}

/// A locked local state directory for one repository/run pair.
#[derive(Debug)]
pub struct RunStore {
    path: PathBuf,
    _lock: File,
}

impl RunStore {
    /// Return `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`.
    pub fn default_state_root() -> Result<PathBuf, RunStoreError> {
        let root = match env::var_os("XDG_STATE_HOME") {
            Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
            _ => {
                let home = env::var_os("HOME").ok_or_else(|| RunStoreError::Io {
                    action: "resolve XDG state root",
                    path: PathBuf::from("$HOME"),
                    message: "HOME is not set".to_owned(),
                })?;
                PathBuf::from(home).join(".local/state")
            }
        };
        Ok(root.join(APPLICATION_DIRECTORY))
    }

    /// Derive the local path without retaining a raw repository path in it.
    pub fn path_for(
        state_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
    ) -> Result<PathBuf, RunStoreError> {
        let repository_id = repository_id(repository)?;
        let run_id = RunId::parse(run_id.as_ref())?;
        Ok(state_root
            .as_ref()
            .join(REPOSITORIES_DIRECTORY)
            .join(repository_id)
            .join(RUNS_DIRECTORY)
            .join(run_id.as_str()))
    }

    /// Create a new locked run.  `state_root` is injectable for tests; the
    /// normal caller should pass [`Self::default_state_root`].
    pub fn create(
        state_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
        history: &History,
        projection: &ActiveProjection,
    ) -> Result<Self, RunStoreError> {
        validate_projection(history, projection)?;
        let path = Self::path_for(state_root, repository, run_id)?;
        create_run_directory(&path)?;
        let lock = acquire_lock(&path)?;
        let store = Self { path, _lock: lock };

        let initial_segment = store.history_path().join(segment_name(history.records())?);
        write_new_file(
            &initial_segment,
            &history.to_jsonl()?,
            "write history segment",
        )?;
        sync_directory(&store.history_path())?;
        store.write_projection(projection)?;
        write_new_file(&store.pending_path(), "", "open pending delta")?;
        sync_directory(&store.path)?;
        Ok(store)
    }

    /// Create a run and atomically persist its private source cursor after the
    /// initial projection is durable.  If a process is interrupted between
    /// those writes, [`Self::recover`] reports `source_cursor: None`; callers
    /// can safely repair that unbound state with
    /// [`Self::commit_projection_with_source_cursor`] and the same projection.
    pub fn create_with_source_cursor(
        state_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
        history: &History,
        projection: &ActiveProjection,
        source_cursor: &SourceCursor,
    ) -> Result<Self, RunStoreError> {
        source_cursor.validate()?;
        let store = Self::create(state_root, repository, run_id, history, projection)?;
        store.write_source_cursor(source_cursor)?;
        Ok(store)
    }

    /// Open an existing run and acquire its exclusive writer lock.
    pub fn open(
        state_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
    ) -> Result<Self, RunStoreError> {
        let path = Self::path_for(state_root, repository, run_id)?;
        verify_run_directory(&path)?;
        let lock = acquire_lock(&path)?;
        let store = Self { path, _lock: lock };
        store.recover()?;
        Ok(store)
    }

    /// Open a run using the current process's XDG state root.
    pub fn open_default(
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
    ) -> Result<Self, RunStoreError> {
        Self::open(Self::default_state_root()?, repository, run_id)
    }

    /// Create a run using the current process's XDG state root.
    pub fn create_default(
        repository: impl AsRef<Path>,
        run_id: impl AsRef<str>,
        history: &History,
        projection: &ActiveProjection,
    ) -> Result<Self, RunStoreError> {
        Self::create(
            Self::default_state_root()?,
            repository,
            run_id,
            history,
            projection,
        )
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically bind this run to the exact isolated child used by `continue`.
    pub fn bind_candidate(
        &self,
        child_thread_id: impl Into<String>,
        candidate: impl AsRef<Path>,
        approved_goal: &str,
    ) -> Result<CandidateBinding, RunStoreError> {
        let child_thread_id = child_thread_id.into();
        if child_thread_id.trim().is_empty()
            || child_thread_id.len() > MAX_SOURCE_CURSOR_IDENTIFIER_BYTES
            || child_thread_id.chars().any(char::is_control)
        {
            return Err(RunStoreError::InvalidStateComponent {
                path: PathBuf::from("candidate child thread id"),
            });
        }
        let workspace_root = fs::canonicalize(self.path.join("workspaces")).map_err(|error| {
            io_error(
                "canonicalize run workspace root",
                &self.path.join("workspaces"),
                error,
            )
        })?;
        let candidate = fs::canonicalize(candidate.as_ref())
            .map_err(|error| io_error("canonicalize bound candidate", candidate.as_ref(), error))?;
        if candidate == workspace_root || !candidate.starts_with(&workspace_root) {
            return Err(RunStoreError::InvalidStateComponent { path: candidate });
        }
        if approved_goal.trim().is_empty() {
            return Err(RunStoreError::InvalidStateComponent {
                path: PathBuf::from("approved child goal"),
            });
        }
        let mut goal_hasher = Sha256::new();
        goal_hasher.update(b"driftctl.approved-child-goal.v1\0");
        goal_hasher.update(approved_goal.as_bytes());
        let binding = CandidateBinding {
            schema_version: CANDIDATE_BINDING_SCHEMA_VERSION,
            child_thread_id,
            candidate_path: candidate,
            approved_goal_digest: format!("sha256:{:x}", goal_hasher.finalize()),
        };
        self.write_candidate_binding(&binding)?;
        Ok(binding)
    }

    /// Load and revalidate the exact isolated candidate bound by `continue`.
    pub fn candidate_binding(&self) -> Result<Option<CandidateBinding>, RunStoreError> {
        let candidate_path = self.candidate_path();
        match fs::symlink_metadata(&candidate_path) {
            Ok(_) => ensure_private_regular(&candidate_path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(io_error(
                    "inspect candidate binding",
                    &candidate_path,
                    error,
                ));
            }
        }
        let input = fs::read_to_string(&candidate_path)
            .map_err(|error| io_error("read candidate binding", &candidate_path, error))?;
        let binding: CandidateBinding = serde_json::from_str(&input)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        if binding.schema_version != CANDIDATE_BINDING_SCHEMA_VERSION
            || binding.child_thread_id.trim().is_empty()
            || binding.child_thread_id.len() > MAX_SOURCE_CURSOR_IDENTIFIER_BYTES
            || binding.child_thread_id.chars().any(char::is_control)
            || !binding.approved_goal_digest.starts_with("sha256:")
        {
            return Err(RunStoreError::InvalidStateComponent {
                path: candidate_path,
            });
        }
        let workspace_root = fs::canonicalize(self.path.join("workspaces")).map_err(|error| {
            io_error(
                "canonicalize run workspace root",
                &self.path.join("workspaces"),
                error,
            )
        })?;
        let canonical = fs::canonicalize(&binding.candidate_path).map_err(|error| {
            io_error(
                "canonicalize bound candidate",
                &binding.candidate_path,
                error,
            )
        })?;
        if canonical != binding.candidate_path
            || canonical == workspace_root
            || !canonical.starts_with(&workspace_root)
        {
            return Err(RunStoreError::InvalidStateComponent {
                path: binding.candidate_path,
            });
        }
        Ok(Some(binding))
    }

    pub fn record_completion_gate(
        &self,
        record: CompletionGateRecord,
    ) -> Result<CompletionGateState, RunStoreError> {
        validate_completion_gate_record(&record, &self.completion_gates_path())?;
        let mut state = self.completion_gate_state()?;
        if record.gate == CompletionGate::Review
            && state
                .record(CompletionGate::Review)
                .is_some_and(|prior| prior.candidate_digest == record.candidate_digest)
        {
            return Err(RunStoreError::ReviewAlreadyRecorded);
        }
        state.records.insert(record.gate, record);
        self.write_completion_gate_state(&state)?;
        Ok(state)
    }

    pub fn completion_gate_state(&self) -> Result<CompletionGateState, RunStoreError> {
        let path = self.completion_gates_path();
        match fs::symlink_metadata(&path) {
            Ok(_) => ensure_private_regular(&path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CompletionGateState::empty());
            }
            Err(error) => return Err(io_error("inspect completion gates", &path, error)),
        }
        let input = fs::read_to_string(&path)
            .map_err(|error| io_error("read completion gates", &path, error))?;
        let state: CompletionGateState = serde_json::from_str(&input)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        if state.schema_version != COMPLETION_GATE_STATE_SCHEMA_VERSION {
            return Err(RunStoreError::InvalidStateComponent { path });
        }
        for (gate, record) in &state.records {
            if *gate != record.gate {
                return Err(RunStoreError::InvalidStateComponent { path });
            }
            validate_completion_gate_record(record, &path)?;
        }
        Ok(state)
    }

    /// Append one validated event to the current unrotated JSONL delta.
    pub fn append_pending(&self, record: EventRecord) -> Result<(), RunStoreError> {
        let recovered = self.recover_projection_state()?;
        let mut records = recovered.history.records().to_vec();
        records.push(record.clone());
        History::replay(records)?;

        let pending = self.pending_path();
        ensure_private_regular_or_missing(&pending)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&pending)
            .map_err(|error| io_error("append pending delta", &pending, error))?;
        serde_json::to_writer(&mut file, &record)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        file.write_all(b"\n")
            .map_err(|error| io_error("append pending delta", &pending, error))?;
        file.flush()
            .map_err(|error| io_error("flush pending delta", &pending, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync pending delta", &pending, error))?;
        sync_directory(&self.path)
    }

    /// Atomically install a projection, then rotate exactly the pending delta
    /// into a never-overwritten immutable history segment.
    pub fn commit_projection(&self, projection: &ActiveProjection) -> Result<(), RunStoreError> {
        let pending = self.read_pending()?;
        if !pending.is_empty() {
            let segment = self.history_path().join(segment_name(&pending)?);
            // Presence is itself the immutable-history error; the existing
            // segment is never opened or trusted on this write path.
            ensure_regular_or_missing(&segment)?;
            if segment.exists() {
                return Err(RunStoreError::ImmutableHistorySegment { path: segment });
            }
        }

        let recovered = self.recover_projection_state()?;
        validate_projection(&recovered.history, projection)?;
        self.write_projection(projection)?;

        if !pending.is_empty() {
            let pending_path = self.pending_path();
            let segment = self.history_path().join(segment_name(&pending)?);
            fs::rename(&pending_path, &segment)
                .map_err(|error| io_error("rotate pending delta", &pending_path, error))?;
            sync_directory(&self.history_path())?;
            write_new_file(&pending_path, "", "open pending delta")?;
            sync_directory(&self.path)?;
        }
        Ok(())
    }

    /// Commit a projection, then atomically advance the private source cursor.
    /// The cursor is not written when projection validation or durability fails.
    pub fn commit_projection_with_source_cursor(
        &self,
        projection: &ActiveProjection,
        source_cursor: &SourceCursor,
    ) -> Result<(), RunStoreError> {
        source_cursor.validate()?;
        self.commit_projection(projection)?;
        self.write_source_cursor(source_cursor)
    }

    /// Read the durable projection and any unrotated JSONL delta.  Interrupted
    /// temporary files are deliberately not part of this lookup.
    pub fn recover(&self) -> Result<RecoveredRun, RunStoreError> {
        let mut recovered = self.recover_projection_state()?;
        recovered.source_cursor = self.read_source_cursor()?;
        Ok(recovered)
    }

    fn recover_projection_state(&self) -> Result<RecoveredRun, RunStoreError> {
        verify_run_directory(&self.path)?;
        let accepted_records = self.read_history_segments()?;
        let accepted_history = History::replay(accepted_records)?;
        let pending_delta = self.read_pending()?;
        let mut all_records = accepted_history.records().to_vec();
        all_records.extend(pending_delta.iter().cloned());
        let history = History::replay(all_records)?;

        let projection_path = self.projection_path();
        ensure_private_regular(&projection_path)?;
        let contents = fs::read_to_string(&projection_path)
            .map_err(|error| io_error("read projection", &projection_path, error))?;
        let projection: ActiveProjection = serde_json::from_str(&contents)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;

        let budget = projection.overflow.budget;
        let mut accepted_projection = project(&accepted_history, budget)?;
        accepted_projection.generated_by = projection.generated_by.clone();
        let mut current_projection = project(&history, budget)?;
        current_projection.generated_by = projection.generated_by.clone();
        if projection != accepted_projection && projection != current_projection {
            return Err(RunStoreError::ProjectionMismatch);
        }
        Ok(RecoveredRun {
            projection,
            history,
            pending_delta,
            source_cursor: None,
        })
    }

    fn write_projection(&self, projection: &ActiveProjection) -> Result<(), RunStoreError> {
        let projection_path = self.projection_path();
        ensure_private_regular_or_missing(&projection_path)?;
        let bytes = serde_json::to_vec(projection)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        let temporary = self.temporary_projection_path();
        write_new_file_bytes(&temporary, &bytes, "write temporary projection")?;
        fs::rename(&temporary, &projection_path)
            .map_err(|error| io_error("replace projection", &projection_path, error))?;
        sync_directory(&self.path)
    }

    fn write_source_cursor(&self, source_cursor: &SourceCursor) -> Result<(), RunStoreError> {
        let source_path = self.source_path();
        ensure_private_regular_or_missing(&source_path)?;
        let bytes = serde_json::to_vec(source_cursor)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        let temporary = self.temporary_source_path();
        write_new_private_file_bytes(&temporary, &bytes, "write temporary source cursor")?;
        fs::rename(&temporary, &source_path)
            .map_err(|error| io_error("replace source cursor", &source_path, error))?;
        sync_directory(&self.path)
    }

    fn write_candidate_binding(&self, binding: &CandidateBinding) -> Result<(), RunStoreError> {
        let candidate_path = self.candidate_path();
        ensure_private_regular_or_missing(&candidate_path)?;
        let bytes = serde_json::to_vec(binding)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        let temporary = self.temporary_candidate_path();
        write_new_private_file_bytes(&temporary, &bytes, "write temporary candidate binding")?;
        fs::rename(&temporary, &candidate_path)
            .map_err(|error| io_error("replace candidate binding", &candidate_path, error))?;
        sync_directory(&self.path)
    }

    fn write_completion_gate_state(
        &self,
        state: &CompletionGateState,
    ) -> Result<(), RunStoreError> {
        let path = self.completion_gates_path();
        ensure_private_regular_or_missing(&path)?;
        let bytes = serde_json::to_vec(state)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        let temporary = self.temporary_completion_gates_path();
        write_new_private_file_bytes(&temporary, &bytes, "write temporary completion gates")?;
        fs::rename(&temporary, &path)
            .map_err(|error| io_error("replace completion gates", &path, error))?;
        sync_directory(&self.path)
    }

    fn read_history_segments(&self) -> Result<Vec<EventRecord>, RunStoreError> {
        let history = self.history_path();
        ensure_private_directory(&history)?;
        let mut paths = fs::read_dir(&history)
            .map_err(|error| io_error("read history directory", &history, error))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| io_error("read history entry", &history, error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();

        let mut records = Vec::new();
        for path in paths {
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                return Err(RunStoreError::InvalidStateComponent { path });
            }
            ensure_private_regular(&path)?;
            let input = fs::read_to_string(&path)
                .map_err(|error| io_error("read history segment", &path, error))?;
            let segment_records = parse_jsonl(&input)?;
            let expected_name = segment_name(&segment_records)?;
            if path
                .file_name()
                .is_none_or(|name| name != std::ffi::OsStr::new(&expected_name))
            {
                return Err(RunStoreError::InvalidStateComponent { path });
            }
            records.extend(segment_records);
        }
        Ok(records)
    }

    fn read_pending(&self) -> Result<Vec<EventRecord>, RunStoreError> {
        let pending = self.pending_path();
        match fs::symlink_metadata(&pending) {
            Ok(_) => ensure_private_regular(&pending)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error("inspect pending delta", &pending, error)),
        }
        let input = fs::read_to_string(&pending)
            .map_err(|error| io_error("read pending delta", &pending, error))?;
        parse_jsonl(&input)
    }

    fn read_source_cursor(&self) -> Result<Option<SourceCursor>, RunStoreError> {
        let source_path = self.source_path();
        match fs::symlink_metadata(&source_path) {
            Ok(_) => ensure_private_regular(&source_path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("inspect source cursor", &source_path, error)),
        }
        let input = fs::read_to_string(&source_path)
            .map_err(|error| io_error("read source cursor", &source_path, error))?;
        let source_cursor: SourceCursor = serde_json::from_str(&input)
            .map_err(|error| RunStoreError::Serialization(error.to_string()))?;
        source_cursor.validate()?;
        Ok(Some(source_cursor))
    }

    fn history_path(&self) -> PathBuf {
        self.path.join(HISTORY_DIRECTORY)
    }

    fn projection_path(&self) -> PathBuf {
        self.path.join(PROJECTION_FILE)
    }

    fn source_path(&self) -> PathBuf {
        self.path.join(SOURCE_FILE)
    }

    fn candidate_path(&self) -> PathBuf {
        self.path.join(CANDIDATE_FILE)
    }

    fn completion_gates_path(&self) -> PathBuf {
        self.path.join(COMPLETION_GATES_FILE)
    }

    fn pending_path(&self) -> PathBuf {
        self.path.join(PENDING_FILE)
    }

    fn temporary_projection_path(&self) -> PathBuf {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.path.join(format!(
            "{TEMP_PROJECTION_PREFIX}{}-{sequence}",
            std::process::id()
        ))
    }

    fn temporary_source_path(&self) -> PathBuf {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.path.join(format!(
            "{TEMP_SOURCE_PREFIX}{}-{sequence}",
            std::process::id()
        ))
    }

    fn temporary_candidate_path(&self) -> PathBuf {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.path.join(format!(
            "{TEMP_CANDIDATE_PREFIX}{}-{sequence}",
            std::process::id()
        ))
    }

    fn temporary_completion_gates_path(&self) -> PathBuf {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.path.join(format!(
            "{TEMP_COMPLETION_GATES_PREFIX}{}-{sequence}",
            std::process::id()
        ))
    }
}

fn validate_completion_gate_record(
    record: &CompletionGateRecord,
    path: &Path,
) -> Result<(), RunStoreError> {
    let fields = [
        record.status.as_str(),
        record.candidate_digest.as_str(),
        record.artifact_id.as_str(),
        record.command_digest.as_str(),
        record.verifier_digest.as_str(),
        record.stdout_digest.as_str(),
        record.stderr_digest.as_str(),
    ];
    let valid_status = matches!(
        record.status.as_str(),
        "passed" | "failed" | "protected_input_changed" | "candidate_changed"
    );
    if !valid_status
        || record.passed != (record.status == "passed")
        || fields.iter().any(|field| {
            field.trim().is_empty()
                || field.len() > MAX_SOURCE_CURSOR_IDENTIFIER_BYTES
                || field.chars().any(char::is_control)
        })
        || !record.candidate_digest.starts_with("sha256:")
    {
        return Err(RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Return a stable SHA-256 repository identity based on its canonical local
/// path.  The path itself is not persisted in the state layout.
pub fn repository_id(repository: impl AsRef<Path>) -> Result<String, RunStoreError> {
    let repository = repository.as_ref();
    let canonical = fs::canonicalize(repository)
        .map_err(|error| io_error("canonicalize repository", repository, error))?;
    let mut digest = Sha256::new();
    digest.update(b"driftctl.repository.v1\0");
    digest.update(canonical.as_os_str().as_encoded_bytes());
    let digest = digest.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_projection(
    history: &History,
    projection: &ActiveProjection,
) -> Result<(), RunStoreError> {
    let mut expected = project(history, projection.overflow.budget)?;
    // Resolver provenance is observational metadata, not folded intent. It
    // survives persistence without weakening any semantic field comparison.
    expected.generated_by = projection.generated_by.clone();
    if &expected == projection {
        Ok(())
    } else {
        Err(RunStoreError::ProjectionMismatch)
    }
}

fn parse_jsonl(input: &str) -> Result<Vec<EventRecord>, RunStoreError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|error| RunStoreError::Serialization(error.to_string()))
        })
        .collect()
}

fn create_run_directory(path: &Path) -> Result<(), RunStoreError> {
    let repositories = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })?;
    ensure_private_directory(repositories.parent().ok_or_else(|| {
        RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        }
    })?)?;
    ensure_private_directory(repositories)?;
    ensure_private_directory(path.parent().and_then(Path::parent).ok_or_else(|| {
        RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        }
    })?)?;
    ensure_private_directory(path.parent().ok_or_else(|| {
        RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        }
    })?)?;

    match fs::create_dir(path) {
        Ok(()) => set_private_directory_permissions(path)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            ensure_not_symlink(path)?;
            return Err(RunStoreError::RunAlreadyExists);
        }
        Err(error) => return Err(io_error("create run directory", path, error)),
    }
    ensure_private_directory(&path.join(HISTORY_DIRECTORY))
}

fn verify_run_directory(path: &Path) -> Result<(), RunStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })?;
    let repository = parent
        .parent()
        .ok_or_else(|| RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })?;
    let repositories = repository
        .parent()
        .ok_or_else(|| RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })?;
    let state_root = repositories
        .parent()
        .ok_or_else(|| RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })?;
    ensure_private_directory(state_root)?;
    ensure_private_directory(repositories)?;
    ensure_private_directory(repository)?;
    ensure_private_directory(parent)?;
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_directory(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RunStoreError::RunNotFound);
        }
        Err(error) => return Err(io_error("inspect run directory", path, error)),
    }
    ensure_private_directory(&path.join(HISTORY_DIRECTORY))
}

fn acquire_lock(run_path: &Path) -> Result<File, RunStoreError> {
    let lock = run_path.join(LOCK_FILE);
    ensure_private_regular_or_missing(&lock)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let file = options
        .open(&lock)
        .map_err(|error| io_error("open writer lock", &lock, error))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(RunStoreError::Busy),
        Err(error) => Err(io_error("acquire writer lock", &lock, error)),
    }
}

fn ensure_directory(path: &Path) -> Result<(), RunStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RunStoreError::SymlinkRefused {
            path: path.to_path_buf(),
        }),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && parent != path
            {
                ensure_directory(parent)?;
            }
            fs::create_dir(path).map_err(|error| io_error("create state directory", path, error))
        }
        Err(error) => Err(io_error("inspect state directory", path, error)),
    }
}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<(), RunStoreError> {
    ensure_directory(path)?;
    set_private_directory_permissions(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), RunStoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("set private state directory permissions", path, error))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), RunStoreError> {
    Ok(())
}

fn ensure_not_symlink(path: &Path) -> Result<(), RunStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect state component", path, error))?;
    if metadata.file_type().is_symlink() {
        Err(RunStoreError::SymlinkRefused {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn ensure_regular(path: &Path) -> Result<(), RunStoreError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("inspect state file", path, error))?;
    if metadata.file_type().is_symlink() {
        Err(RunStoreError::SymlinkRefused {
            path: path.to_path_buf(),
        })
    } else if metadata.is_file() {
        Ok(())
    } else {
        Err(RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        })
    }
}

fn ensure_regular_or_missing(path: &Path) -> Result<(), RunStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RunStoreError::SymlinkRefused {
            path: path.to_path_buf(),
        }),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(RunStoreError::InvalidStateComponent {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect state file", path, error)),
    }
}

fn ensure_private_regular(path: &Path) -> Result<(), RunStoreError> {
    ensure_regular(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = fs::metadata(path)
            .map_err(|error| io_error("inspect private state file", path, error))?
            .permissions();
        if permissions.mode() & 0o077 != 0 {
            return Err(RunStoreError::PrivateStatePermissions {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn ensure_private_regular_or_missing(path: &Path) -> Result<(), RunStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_regular(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect private state file", path, error)),
    }
}

fn write_new_file(path: &Path, contents: &str, action: &'static str) -> Result<(), RunStoreError> {
    write_new_file_bytes(path, contents.as_bytes(), action)
}

fn write_new_file_bytes(
    path: &Path,
    contents: &[u8],
    action: &'static str,
) -> Result<(), RunStoreError> {
    ensure_regular_or_missing(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            RunStoreError::ImmutableHistorySegment {
                path: path.to_path_buf(),
            }
        } else {
            io_error(action, path, error)
        }
    })?;
    file.write_all(contents)
        .map_err(|error| io_error(action, path, error))?;
    file.flush()
        .map_err(|error| io_error("flush state file", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync state file", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("set private state permissions", path, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync private state permissions", path, error))?;
    }
    Ok(())
}

fn write_new_private_file_bytes(
    path: &Path,
    contents: &[u8],
    action: &'static str,
) -> Result<(), RunStoreError> {
    ensure_regular_or_missing(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| io_error(action, path, error))?;
    file.write_all(contents)
        .map_err(|error| io_error(action, path, error))?;
    file.flush()
        .map_err(|error| io_error("flush private state file", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync private state file", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("set private state permissions", path, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync private state permissions", path, error))?;
    }
    Ok(())
}

fn segment_name(records: &[EventRecord]) -> Result<String, RunStoreError> {
    let Some(first) = records.first() else {
        return Err(RunStoreError::InvalidStateComponent {
            path: PathBuf::from(HISTORY_DIRECTORY),
        });
    };
    let last = records.last().expect("checked nonempty records");
    Ok(format!(
        "{:020}-{:020}.jsonl",
        first.sequence, last.sequence
    ))
}

fn sync_directory(path: &Path) -> Result<(), RunStoreError> {
    #[cfg(unix)]
    {
        let directory =
            File::open(path).map_err(|error| io_error("open state directory", path, error))?;
        match directory.sync_all() {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
                ) => {}
            Err(error) => return Err(io_error("sync state directory", path, error)),
        }
    }
    Ok(())
}

fn io_error(action: &'static str, path: &Path, error: io::Error) -> RunStoreError {
    RunStoreError::Io {
        action,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn validate_source_identifier(value: &str, field: &'static str) -> Result<(), SourceCursorError> {
    if value.trim().is_empty()
        || value.len() > MAX_SOURCE_CURSOR_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        Err(SourceCursorError::InvalidField(field))
    } else {
        Ok(())
    }
}

fn validate_source_digest(value: &str, field: &'static str) -> Result<(), SourceCursorError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(SourceCursorError::InvalidDigest(field));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SourceCursorError::InvalidDigest(field));
    }
    Ok(())
}

fn source_cursor_digest(records: &[SourceRecordDigest]) -> String {
    let mut digest = Sha256::new();
    for record in records {
        digest.update(record.id.as_bytes());
        digest.update([0]);
        digest.update(record.content_digest.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}
