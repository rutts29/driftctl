//! Private durable state for the one pending goal change owned by an inspect run.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::intent_history::SourceRef;
use crate::run_store::RunStore;
use crate::semantic_resolver::GoalChangeObservation;

const SCHEMA_VERSION: u32 = 1;
const PROPOSALS_DIRECTORY: &str = "proposals";
const GOAL_CHANGE_FILE: &str = "goal-change.json";
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingGoalChange {
    schema_version: u32,
    revision: u64,
    status: GoalChangeStatus,
    proposed_goal: String,
    source_refs: Vec<SourceRef>,
    base_projection_revision: u64,
    base_event_sequence: u64,
    source_digest: String,
}

impl PendingGoalChange {
    pub(crate) fn new(
        observation: &GoalChangeObservation,
        source_digest: impl Into<String>,
    ) -> Result<Self, GoalChangeStoreError> {
        let proposal = Self {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            status: GoalChangeStatus::Pending,
            proposed_goal: observation.proposed_goal.clone(),
            source_refs: observation.source_refs.clone(),
            base_projection_revision: observation.base_projection_revision,
            base_event_sequence: observation.base_event_sequence,
            source_digest: source_digest.into(),
        };
        proposal.validate()?;
        Ok(proposal)
    }

    pub(crate) fn observation(&self) -> GoalChangeObservation {
        GoalChangeObservation {
            proposed_goal: self.proposed_goal.clone(),
            source_refs: self.source_refs.clone(),
            base_projection_revision: self.base_projection_revision,
            base_event_sequence: self.base_event_sequence,
        }
    }

    pub(crate) fn proposed_goal(&self) -> &str {
        &self.proposed_goal
    }

    pub(crate) fn source_refs(&self) -> &[SourceRef] {
        &self.source_refs
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.status == GoalChangeStatus::Pending
    }

    pub(crate) fn matches(
        &self,
        projection_revision: u64,
        event_sequence: u64,
        source_digest: &str,
    ) -> bool {
        self.base_projection_revision == projection_revision
            && self.base_event_sequence == event_sequence
            && self.source_digest == source_digest
    }

    fn validate(&self) -> Result<(), GoalChangeStoreError> {
        if self.schema_version != SCHEMA_VERSION || self.revision == 0 {
            return Err(GoalChangeStoreError::Invalid("unsupported schema version"));
        }
        if self.proposed_goal.trim().is_empty()
            || self.source_refs.is_empty()
            || self.base_projection_revision == 0
            || self.base_event_sequence == 0
            || self.source_digest.trim().is_empty()
        {
            return Err(GoalChangeStoreError::Invalid(
                "invalid goal-change proposal",
            ));
        }
        self.source_refs
            .iter()
            .try_for_each(SourceRef::validate)
            .map_err(|_| GoalChangeStoreError::Invalid("invalid goal-change source reference"))
    }

    fn edit(&self, proposed_goal: &str) -> Result<Self, GoalChangeStoreError> {
        let mut edited = self.clone();
        edited.revision = edited.revision.saturating_add(1);
        edited.status = GoalChangeStatus::Pending;
        edited.proposed_goal = proposed_goal.trim().to_owned();
        edited.validate()?;
        Ok(edited)
    }

    fn with_status(&self, status: GoalChangeStatus) -> Self {
        let mut decided = self.clone();
        decided.status = status;
        decided
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GoalChangeStatus {
    Pending,
    Rejected,
    Applied,
}

pub(crate) struct GoalChangeStore {
    directory: PathBuf,
}

impl GoalChangeStore {
    pub(crate) fn open(run: &RunStore) -> Result<Self, GoalChangeStoreError> {
        let directory = run.path().join(PROPOSALS_DIRECTORY);
        ensure_private_directory(&directory)?;
        Ok(Self { directory })
    }

    pub(crate) fn load(&self) -> Result<Option<PendingGoalChange>, GoalChangeStoreError> {
        let path = self.path();
        let Some(contents) = read_private_regular_if_present(&path)? else {
            return Ok(None);
        };
        let proposal: PendingGoalChange = serde_json::from_slice(&contents)
            .map_err(|_| GoalChangeStoreError::Invalid("malformed goal-change proposal"))?;
        proposal.validate()?;
        Ok(Some(proposal))
    }

    pub(crate) fn load_pending(&self) -> Result<Option<PendingGoalChange>, GoalChangeStoreError> {
        Ok(self.load()?.filter(PendingGoalChange::is_pending))
    }

    pub(crate) fn persist(&self, proposal: &PendingGoalChange) -> Result<(), GoalChangeStoreError> {
        proposal.validate()?;
        if let Some(existing) = self.load()? {
            if existing == *proposal {
                return Ok(());
            }
            if existing.is_pending() {
                return Err(GoalChangeStoreError::Invalid(
                    "a different goal-change proposal is already pending",
                ));
            }
            let mut next = proposal.clone();
            next.revision = existing.revision.saturating_add(1);
            return self.replace(&next);
        }
        let bytes = serde_json::to_vec(proposal)
            .map_err(|_| GoalChangeStoreError::Invalid("could not encode goal-change proposal"))?;
        write_private_atomic(&self.directory, GOAL_CHANGE_FILE, &bytes)
    }

    pub(crate) fn edit(
        &self,
        current: &PendingGoalChange,
        proposed_goal: &str,
    ) -> Result<PendingGoalChange, GoalChangeStoreError> {
        self.require_current(current)?;
        let edited = current.edit(proposed_goal)?;
        self.replace(&edited)?;
        Ok(edited)
    }

    pub(crate) fn reject(&self, current: &PendingGoalChange) -> Result<(), GoalChangeStoreError> {
        self.require_current(current)?;
        self.replace(&current.with_status(GoalChangeStatus::Rejected))
    }

    pub(crate) fn mark_applied(
        &self,
        current: &PendingGoalChange,
    ) -> Result<(), GoalChangeStoreError> {
        self.require_current(current)?;
        self.replace(&current.with_status(GoalChangeStatus::Applied))
    }

    fn require_current(&self, expected: &PendingGoalChange) -> Result<(), GoalChangeStoreError> {
        match self.load()? {
            Some(current) if current == *expected && current.is_pending() => Ok(()),
            _ => Err(GoalChangeStoreError::Invalid(
                "goal-change decision is stale or already resolved",
            )),
        }
    }

    fn replace(&self, proposal: &PendingGoalChange) -> Result<(), GoalChangeStoreError> {
        proposal.validate()?;
        let bytes = serde_json::to_vec(proposal)
            .map_err(|_| GoalChangeStoreError::Invalid("could not encode goal-change proposal"))?;
        write_private_atomic(&self.directory, GOAL_CHANGE_FILE, &bytes)
    }

    fn path(&self) -> PathBuf {
        self.directory.join(GOAL_CHANGE_FILE)
    }
}

#[derive(Debug)]
pub(crate) enum GoalChangeStoreError {
    Io(&'static str, std::io::Error),
    Invalid(&'static str),
}

impl fmt::Display for GoalChangeStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(action, error) => write!(formatter, "{action}: {error}"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GoalChangeStoreError {}

fn ensure_private_directory(path: &Path) -> Result<(), GoalChangeStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => check_private_mode(&metadata),
        Ok(_) => Err(GoalChangeStoreError::Invalid(
            "proposal state path is not a private directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|error| GoalChangeStoreError::Io("create proposal directory", error))?;
            set_private_directory_mode(path)?;
            sync_directory(path.parent().expect("proposal directory has parent"))
        }
        Err(error) => Err(GoalChangeStoreError::Io(
            "inspect proposal directory",
            error,
        )),
    }
}

fn read_private_regular_if_present(path: &Path) -> Result<Option<Vec<u8>>, GoalChangeStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GoalChangeStoreError::Io("inspect proposal file", error)),
    };
    if !metadata.file_type().is_file() {
        return Err(GoalChangeStoreError::Invalid(
            "proposal state path is not a private regular file",
        ));
    }
    check_private_mode(&metadata)?;
    fs::read(path)
        .map(Some)
        .map_err(|error| GoalChangeStoreError::Io("read proposal file", error))
}

fn write_private_atomic(
    directory: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), GoalChangeStoreError> {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_options(&mut options);
    let mut file = options
        .open(&temporary)
        .map_err(|error| GoalChangeStoreError::Io("create temporary proposal", error))?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| GoalChangeStoreError::Io("write temporary proposal", error))
        .and_then(|()| {
            fs::rename(&temporary, directory.join(name))
                .map_err(|error| GoalChangeStoreError::Io("install proposal", error))
        })
        .and_then(|()| sync_directory(directory));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn sync_directory(path: &Path) -> Result<(), GoalChangeStoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| GoalChangeStoreError::Io("sync proposal directory", error))
}

#[cfg(unix)]
fn set_private_file_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_options(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), GoalChangeStoreError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| GoalChangeStoreError::Io("set proposal directory permissions", error))
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<(), GoalChangeStoreError> {
    Ok(())
}

#[cfg(unix)]
fn check_private_mode(metadata: &fs::Metadata) -> Result<(), GoalChangeStoreError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(GoalChangeStoreError::Invalid(
            "proposal state permissions are not private",
        ))
    }
}

#[cfg(not(unix))]
fn check_private_mode(_metadata: &fs::Metadata) -> Result<(), GoalChangeStoreError> {
    Ok(())
}
