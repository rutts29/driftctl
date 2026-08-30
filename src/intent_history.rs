//! Typed, append-only intent history for the v2 domain model.
//!
//! This module deliberately stops at the domain/history boundary.  It does
//! not build a prompt projection, call a model, or create local state.  The
//! v1 [`crate::ledger::Ledger`] remains the compatibility ledger used by the
//! existing CLI.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// The exact event schema accepted by this history implementation.
pub const HISTORY_SCHEMA_VERSION: u32 = 2;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, HistoryError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(HistoryError::InvalidTransition {
                        event: "id",
                        reason: "identifier must not be empty".to_owned(),
                    });
                }
                Ok(Self(value))
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

typed_id!(IntentId);
typed_id!(ConflictId);
typed_id!(EvidenceId);

/// A source provider whose records can be referred to by an intent event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceProvider {
    Codex,
    Bundle,
}

/// The role that produced a source record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    User,
    Assistant,
    Tool,
    SystemObservation,
}

/// A private, source-linked reference to an imported provider record.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRef {
    pub provider: SourceProvider,
    /// This value is intentionally local/private and must not be included in
    /// a sanitized public export by later layers.
    pub session: String,
    pub record: String,
    pub role: SourceRole,
    pub content_digest: String,
}

impl fmt::Debug for SourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRef")
            .field("provider", &self.provider)
            .field("record", &self.record)
            .field("role", &self.role)
            .field("content_digest", &self.content_digest)
            .finish_non_exhaustive()
    }
}

impl SourceRef {
    #[must_use]
    pub fn new(
        provider: SourceProvider,
        session: impl Into<String>,
        record: impl Into<String>,
        role: SourceRole,
        content_digest: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            session: session.into(),
            record: record.into(),
            role,
            content_digest: content_digest.into(),
        }
    }

    /// The session locator is private by contract; this named accessor keeps
    /// that boundary visible to callers without changing the persisted field.
    #[must_use]
    pub fn session_private(&self) -> &str {
        &self.session
    }

    pub fn validate(&self) -> Result<(), HistoryError> {
        non_empty(&self.session, "source session")?;
        non_empty(&self.record, "source record")?;
        non_empty(&self.content_digest, "source content digest")
    }
}

/// An explicit operator approval source.  Assistant and tool output alone
/// never carries semantic authority; it must be accompanied by this value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Approval {
    pub source_refs: Vec<SourceRef>,
}

impl Approval {
    #[must_use]
    pub fn new(source: SourceRef) -> Self {
        Self {
            source_refs: vec![source],
        }
    }

    #[must_use]
    pub fn from_sources(source_refs: Vec<SourceRef>) -> Self {
        Self { source_refs }
    }

    fn validate(&self, event: &'static str) -> Result<(), HistoryError> {
        validate_source_refs(&self.source_refs, event)?;
        if !self.source_refs.iter().any(|source| {
            matches!(
                source.role,
                SourceRole::User | SourceRole::SystemObservation
            )
        }) {
            return Err(HistoryError::InvalidApproval { event });
        }
        Ok(())
    }
}

/// The semantic category of an intent item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    Outcome,
    Constraint,
    Invariant,
    Scope,
    Validation,
    StopCondition,
}

/// Lifecycle is intentionally independent of [`EvidenceState`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentLifecycle {
    Active,
    Superseded,
    Withdrawn,
    Conflicted,
}

/// Evidence state is intentionally independent of [`IntentLifecycle`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Unresolved,
    Satisfied,
    Reopened,
}

/// Evidence attached to an intent item.  Its source may be assistant/tool
/// output because evidence is not itself semantic intent authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRef {
    pub id: EvidenceId,
    pub summary: String,
    pub source_refs: Vec<SourceRef>,
}

impl EvidenceRef {
    #[must_use]
    pub fn new(
        id: impl Into<EvidenceId>,
        summary: impl Into<String>,
        source_refs: Vec<SourceRef>,
    ) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            source_refs,
        }
    }
}

/// A source-linked intent item retained in history even after it is inactive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IntentItem {
    pub id: IntentId,
    pub kind: IntentKind,
    pub text: String,
    pub lifecycle: IntentLifecycle,
    pub evidence_state: EvidenceState,
    pub introduced_by: Vec<SourceRef>,
    pub changed_by: Vec<SourceRef>,
    pub supersedes: Vec<IntentId>,
    pub evidence: Vec<EvidenceRef>,
}

impl IntentItem {
    #[must_use]
    pub fn new(
        id: impl Into<IntentId>,
        kind: IntentKind,
        text: impl Into<String>,
        introduced_by: Vec<SourceRef>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            lifecycle: IntentLifecycle::Active,
            evidence_state: EvidenceState::Unresolved,
            introduced_by,
            changed_by: Vec::new(),
            supersedes: Vec::new(),
            evidence: Vec::new(),
        }
    }

    /// Construct the replacement side of a supersession edge.
    #[must_use]
    pub fn superseding(
        id: impl Into<IntentId>,
        kind: IntentKind,
        text: impl Into<String>,
        introduced_by: Vec<SourceRef>,
        previous_id: impl Into<IntentId>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            lifecycle: IntentLifecycle::Active,
            evidence_state: EvidenceState::Unresolved,
            changed_by: introduced_by.clone(),
            introduced_by,
            supersedes: vec![previous_id.into()],
            evidence: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self.lifecycle, IntentLifecycle::Active)
    }
}

/// An approved, source-linked revision of the native/projected goal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoalRevision {
    pub revision: u64,
    pub text: String,
    pub source_refs: Vec<SourceRef>,
    pub supersedes_revision: Option<u64>,
    pub approval: Option<Approval>,
}

impl GoalRevision {
    #[must_use]
    pub fn new(revision: u64, text: impl Into<String>, source_refs: Vec<SourceRef>) -> Self {
        Self {
            revision,
            text: text.into(),
            source_refs,
            supersedes_revision: None,
            approval: None,
        }
    }
}

/// One alternative in a semantic conflict.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConflictAlternative {
    pub id: String,
    pub text: String,
    pub source_refs: Vec<SourceRef>,
}

impl ConflictAlternative {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        source_refs: Vec<SourceRef>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            source_refs,
        }
    }
}

/// A source-linked conflict affecting one or more active intent items.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Conflict {
    pub id: ConflictId,
    pub intent_ids: Vec<IntentId>,
    pub alternatives: Vec<ConflictAlternative>,
    pub source_refs: Vec<SourceRef>,
}

impl Conflict {
    #[must_use]
    pub fn new(
        id: impl Into<ConflictId>,
        intent_ids: Vec<IntentId>,
        alternatives: Vec<ConflictAlternative>,
        source_refs: Vec<SourceRef>,
    ) -> Self {
        Self {
            id: id.into(),
            intent_ids,
            alternatives,
            source_refs,
        }
    }
}

/// A source-linked choice that resolves one live conflict.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConflictResolution {
    pub alternative_id: String,
    pub source_refs: Vec<SourceRef>,
    pub approval: Option<Approval>,
}

impl ConflictResolution {
    #[must_use]
    pub fn new(
        alternative_id: impl Into<String>,
        source_refs: Vec<SourceRef>,
        approval: Option<Approval>,
    ) -> Self {
        Self {
            alternative_id: alternative_id.into(),
            source_refs,
            approval,
        }
    }
}

/// A domain event.  [`EventRecord`] supplies its immutable schema and
/// sequence envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        goal: GoalRevision,
    },
    RequirementAdded {
        item: IntentItem,
        approval: Option<Approval>,
    },
    RequirementSuperseded {
        previous_id: IntentId,
        replacement: IntentItem,
        approval: Option<Approval>,
    },
    RequirementWithdrawn {
        intent_id: IntentId,
        source_refs: Vec<SourceRef>,
        approval: Option<Approval>,
    },
    ConflictRaised {
        conflict: Conflict,
        approval: Option<Approval>,
    },
    ConflictResolved {
        conflict_id: ConflictId,
        resolution: ConflictResolution,
    },
    GoalRevised {
        goal: GoalRevision,
    },
    EvidenceAttached {
        intent_id: IntentId,
        evidence: EvidenceRef,
    },
    EvidenceInvalidated {
        intent_id: IntentId,
        evidence_id: EvidenceId,
        source_refs: Vec<SourceRef>,
        reason: String,
    },
    RunClosed {
        source_refs: Vec<SourceRef>,
        approval: Option<Approval>,
    },
}

/// An immutable event with an exact schema version and contiguous sequence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventRecord {
    pub schema_version: u32,
    pub sequence: u64,
    #[serde(flatten)]
    pub event: Event,
}

impl EventRecord {
    #[must_use]
    pub fn new(sequence: u64, event: Event) -> Self {
        Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            sequence,
            event,
        }
    }
}

/// A replay failure.  Errors distinguish malformed envelopes from invalid
/// domain transitions so callers can report a useful source-boundary issue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryError {
    EmptyHistory,
    UnsupportedSchemaVersion { expected: u32, found: u32 },
    SequenceGap { expected: u64, found: u64 },
    RunStartedRequired,
    RunStartedNotFirst,
    EventAfterClosure,
    DuplicateId { kind: &'static str, id: String },
    UnknownIntent { id: IntentId },
    UnknownConflict { id: ConflictId },
    UnknownEvidence { id: EvidenceId },
    MissingSourceRefs { event: &'static str },
    InvalidSourceRef { field: &'static str },
    UnauthorizedSource { event: &'static str },
    InvalidApproval { event: &'static str },
    InvalidTransition { event: &'static str, reason: String },
    Serialization(String),
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHistory => formatter.write_str("intent history is empty"),
            Self::UnsupportedSchemaVersion { expected, found } => write!(
                formatter,
                "unsupported intent history schema version: expected {expected}, found {found}"
            ),
            Self::SequenceGap { expected, found } => {
                write!(
                    formatter,
                    "invalid intent history sequence: expected {expected}, found {found}"
                )
            }
            Self::RunStartedRequired => {
                formatter.write_str("intent history must begin with run_started")
            }
            Self::RunStartedNotFirst => formatter.write_str("run_started may only appear first"),
            Self::EventAfterClosure => {
                formatter.write_str("intent history contains an event after closure")
            }
            Self::DuplicateId { kind, id } => write!(formatter, "duplicate {kind} id: {id}"),
            Self::UnknownIntent { id } => write!(formatter, "unknown intent id: {id}"),
            Self::UnknownConflict { id } => write!(formatter, "unknown conflict id: {id}"),
            Self::UnknownEvidence { id } => write!(formatter, "unknown evidence id: {id}"),
            Self::MissingSourceRefs { event } => {
                write!(formatter, "{event} requires nonempty source refs")
            }
            Self::InvalidSourceRef { field } => write!(formatter, "{field} must not be empty"),
            Self::UnauthorizedSource { event } => write!(
                formatter,
                "{event} requires a user source or explicit operator approval"
            ),
            Self::InvalidApproval { event } => {
                write!(formatter, "{event} has no valid explicit approval source")
            }
            Self::InvalidTransition { event, reason } => write!(formatter, "{event}: {reason}"),
            Self::Serialization(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for HistoryError {}

impl From<serde_json::Error> for HistoryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConflictState {
    conflict: Conflict,
    resolved: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReplayState {
    goal: Option<GoalRevision>,
    intents: BTreeMap<IntentId, IntentItem>,
    conflicts: BTreeMap<ConflictId, ConflictState>,
    evidence_invalidated: BTreeSet<EvidenceId>,
    closed: bool,
}

impl ReplayState {
    fn empty() -> Self {
        Self {
            goal: None,
            intents: BTreeMap::new(),
            conflicts: BTreeMap::new(),
            evidence_invalidated: BTreeSet::new(),
            closed: false,
        }
    }
}

/// Current replay state.  Inactive items are retained for auditability;
/// `open_conflicts` contains only unresolved conflicts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntentSnapshot {
    pub goal: GoalRevision,
    pub intents: BTreeMap<IntentId, IntentItem>,
    pub open_conflicts: BTreeMap<ConflictId, Conflict>,
    pub closed: bool,
}

/// An append-only v2 event history with deterministic replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    records: Vec<EventRecord>,
    state: ReplayState,
}

impl History {
    /// Start a new v2 history with its first `run_started` event.
    pub fn new(goal: GoalRevision) -> Result<Self, HistoryError> {
        let mut history = Self {
            records: Vec::new(),
            state: ReplayState::empty(),
        };
        history.append(Event::RunStarted { goal })?;
        Ok(history)
    }

    /// Replay an ordered sequence of versioned events without mutating it.
    pub fn replay<I>(records: I) -> Result<Self, HistoryError>
    where
        I: IntoIterator<Item = EventRecord>,
    {
        let mut history = Self {
            records: Vec::new(),
            state: ReplayState::empty(),
        };
        for record in records {
            history.apply_record(&record)?;
            history.records.push(record);
        }
        if history.records.is_empty() {
            return Err(HistoryError::EmptyHistory);
        }
        Ok(history)
    }

    /// Append one event after validating it against a cloned state.  A failed
    /// event leaves both the state and the ordered records untouched.
    pub fn append(&mut self, event: Event) -> Result<EventRecord, HistoryError> {
        let sequence = u64::try_from(self.records.len())
            .ok()
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| HistoryError::InvalidTransition {
                event: "append",
                reason: "event sequence exhausted".to_owned(),
            })?;
        let record = EventRecord::new(sequence, event);
        self.apply_record(&record)?;
        self.records.push(record.clone());
        Ok(record)
    }

    /// The immutable ordered records, suitable for persistence by a caller.
    #[must_use]
    pub fn records(&self) -> &[EventRecord] {
        &self.records
    }

    /// Alias emphasizing that records are the event stream.
    #[must_use]
    pub fn events(&self) -> &[EventRecord] {
        self.records()
    }

    #[must_use]
    pub fn goal(&self) -> &GoalRevision {
        self.state
            .goal
            .as_ref()
            .expect("a valid history always has a goal")
    }

    #[must_use]
    pub fn intent(&self, id: impl AsRef<str>) -> Option<&IntentItem> {
        self.state
            .intents
            .iter()
            .find(|(intent_id, _)| intent_id.as_str() == id.as_ref())
            .map(|(_, item)| item)
    }

    #[must_use]
    pub fn intents(&self) -> &BTreeMap<IntentId, IntentItem> {
        &self.state.intents
    }

    #[must_use]
    pub fn open_conflicts(&self) -> BTreeMap<ConflictId, Conflict> {
        self.state
            .conflicts
            .iter()
            .filter(|(_, conflict)| !conflict.resolved)
            .map(|(id, conflict)| (id.clone(), conflict.conflict.clone()))
            .collect()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.closed
    }

    #[must_use]
    pub fn snapshot(&self) -> IntentSnapshot {
        IntentSnapshot {
            goal: self.goal().clone(),
            intents: self.state.intents.clone(),
            open_conflicts: self.open_conflicts(),
            closed: self.state.closed,
        }
    }

    /// Serialize the ordered event stream as a JSON array.
    pub fn to_json(&self) -> Result<String, HistoryError> {
        serde_json::to_string(&self.records).map_err(HistoryError::from)
    }

    /// Deserialize and replay a JSON array of ordered event records.
    pub fn from_json(input: &str) -> Result<Self, HistoryError> {
        let records: Vec<EventRecord> = serde_json::from_str(input)?;
        Self::replay(records)
    }

    /// Serialize the ordered event stream as JSONL for append-oriented stores.
    pub fn to_jsonl(&self) -> Result<String, HistoryError> {
        self.records
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map(|lines| format!("{}\n", lines.join("\n")))
            .map_err(HistoryError::from)
    }

    /// Deserialize and replay JSONL records.  A final empty line is accepted
    /// as the normal JSONL trailing newline, while nonempty lines remain
    /// strictly ordered and validated.
    pub fn from_jsonl(input: &str) -> Result<Self, HistoryError> {
        let mut records = Vec::new();
        for (line_number, line) in input.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str(line).map_err(|error| {
                HistoryError::Serialization(format!(
                    "invalid intent history line {}: {error}",
                    line_number + 1
                ))
            })?;
            records.push(record);
        }
        Self::replay(records)
    }

    fn apply_record(&mut self, record: &EventRecord) -> Result<(), HistoryError> {
        let expected = u64::try_from(self.records.len())
            .ok()
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| HistoryError::InvalidTransition {
                event: "replay",
                reason: "event sequence exhausted".to_owned(),
            })?;
        if record.schema_version != HISTORY_SCHEMA_VERSION {
            return Err(HistoryError::UnsupportedSchemaVersion {
                expected: HISTORY_SCHEMA_VERSION,
                found: record.schema_version,
            });
        }
        if record.sequence != expected {
            return Err(HistoryError::SequenceGap {
                expected,
                found: record.sequence,
            });
        }

        let mut next = self.state.clone();
        apply_event(&mut next, &record.event)?;
        self.state = next;
        Ok(())
    }
}

fn apply_event(state: &mut ReplayState, event: &Event) -> Result<(), HistoryError> {
    if state.closed {
        return Err(HistoryError::EventAfterClosure);
    }

    match event {
        Event::RunStarted { goal } => {
            if state.goal.is_some() {
                return Err(HistoryError::RunStartedNotFirst);
            }
            validate_goal(goal, true, "run_started")?;
            if goal.revision != 1 || goal.supersedes_revision.is_some() {
                return Err(HistoryError::InvalidTransition {
                    event: "run_started",
                    reason: "initial goal must be revision 1 without a superseded revision"
                        .to_owned(),
                });
            }
            state.goal = Some(goal.clone());
        }
        Event::RequirementAdded { item, approval } => {
            ensure_started(state, "requirement_added")?;
            validate_new_item(item, approval.as_ref(), "requirement_added")?;
            if state.intents.contains_key(&item.id) {
                return Err(HistoryError::DuplicateId {
                    kind: "intent",
                    id: item.id.to_string(),
                });
            }
            state.intents.insert(item.id.clone(), item.clone());
        }
        Event::RequirementSuperseded {
            previous_id,
            replacement,
            approval,
        } => {
            ensure_started(state, "requirement_superseded")?;
            validate_new_item(replacement, approval.as_ref(), "requirement_superseded")?;
            if replacement.supersedes.as_slice() != [previous_id.clone()] {
                return Err(HistoryError::InvalidTransition {
                    event: "requirement_superseded",
                    reason: "replacement must contain exactly the superseded intent id".to_owned(),
                });
            }
            if state.intents.contains_key(&replacement.id) {
                return Err(HistoryError::DuplicateId {
                    kind: "intent",
                    id: replacement.id.to_string(),
                });
            }
            let old =
                state
                    .intents
                    .get_mut(previous_id)
                    .ok_or_else(|| HistoryError::UnknownIntent {
                        id: previous_id.clone(),
                    })?;
            if !old.is_active() {
                return Err(HistoryError::InvalidTransition {
                    event: "requirement_superseded",
                    reason: "only an active intent can be superseded".to_owned(),
                });
            }
            old.lifecycle = IntentLifecycle::Superseded;
            append_sources(&mut old.changed_by, &replacement.introduced_by);
            let mut replacement = replacement.clone();
            if replacement.changed_by.is_empty() {
                replacement.changed_by = replacement.introduced_by.clone();
            }
            state.intents.insert(replacement.id.clone(), replacement);
        }
        Event::RequirementWithdrawn {
            intent_id,
            source_refs,
            approval,
        } => {
            ensure_started(state, "requirement_withdrawn")?;
            validate_authority(source_refs, approval.as_ref(), "requirement_withdrawn")?;
            let item =
                state
                    .intents
                    .get_mut(intent_id)
                    .ok_or_else(|| HistoryError::UnknownIntent {
                        id: intent_id.clone(),
                    })?;
            if !item.is_active() {
                return Err(HistoryError::InvalidTransition {
                    event: "requirement_withdrawn",
                    reason: "only an active intent can be withdrawn".to_owned(),
                });
            }
            item.lifecycle = IntentLifecycle::Withdrawn;
            append_sources(&mut item.changed_by, source_refs);
            if let Some(approval) = approval {
                append_sources(&mut item.changed_by, &approval.source_refs);
            }
        }
        Event::ConflictRaised { conflict, approval } => {
            ensure_started(state, "conflict_raised")?;
            validate_conflict(conflict, approval.as_ref(), "conflict_raised")?;
            if state.conflicts.contains_key(&conflict.id) {
                return Err(HistoryError::DuplicateId {
                    kind: "conflict",
                    id: conflict.id.to_string(),
                });
            }
            for intent_id in &conflict.intent_ids {
                let item =
                    state
                        .intents
                        .get(intent_id)
                        .ok_or_else(|| HistoryError::UnknownIntent {
                            id: intent_id.clone(),
                        })?;
                if !item.is_active() {
                    return Err(HistoryError::InvalidTransition {
                        event: "conflict_raised",
                        reason: "a conflict may only affect active intents".to_owned(),
                    });
                }
            }
            for intent_id in &conflict.intent_ids {
                let item = state
                    .intents
                    .get_mut(intent_id)
                    .expect("validated intent id exists");
                item.lifecycle = IntentLifecycle::Conflicted;
                append_sources(&mut item.changed_by, &conflict.source_refs);
                if let Some(approval) = approval {
                    append_sources(&mut item.changed_by, &approval.source_refs);
                }
            }
            state.conflicts.insert(
                conflict.id.clone(),
                ConflictState {
                    conflict: conflict.clone(),
                    resolved: false,
                },
            );
        }
        Event::ConflictResolved {
            conflict_id,
            resolution,
        } => {
            ensure_started(state, "conflict_resolved")?;
            validate_authority(
                &resolution.source_refs,
                resolution.approval.as_ref(),
                "conflict_resolved",
            )?;
            non_empty(&resolution.alternative_id, "conflict alternative id")?;
            let conflict =
                state
                    .conflicts
                    .get(conflict_id)
                    .ok_or_else(|| HistoryError::UnknownConflict {
                        id: conflict_id.clone(),
                    })?;
            if conflict.resolved {
                return Err(HistoryError::InvalidTransition {
                    event: "conflict_resolved",
                    reason: "conflict is already resolved".to_owned(),
                });
            }
            if !conflict
                .conflict
                .alternatives
                .iter()
                .any(|alternative| alternative.id == resolution.alternative_id)
            {
                return Err(HistoryError::InvalidTransition {
                    event: "conflict_resolved",
                    reason: "resolution does not select a live alternative".to_owned(),
                });
            }
            let intent_ids = conflict.conflict.intent_ids.clone();
            for intent_id in &intent_ids {
                let item =
                    state
                        .intents
                        .get(intent_id)
                        .ok_or_else(|| HistoryError::UnknownIntent {
                            id: intent_id.clone(),
                        })?;
                if !matches!(item.lifecycle, IntentLifecycle::Conflicted) {
                    return Err(HistoryError::InvalidTransition {
                        event: "conflict_resolved",
                        reason: "affected intent is no longer conflicted".to_owned(),
                    });
                }
            }
            for intent_id in intent_ids {
                let item = state
                    .intents
                    .get_mut(&intent_id)
                    .expect("validated intent id exists");
                item.lifecycle = IntentLifecycle::Active;
                append_sources(&mut item.changed_by, &resolution.source_refs);
                if let Some(approval) = &resolution.approval {
                    append_sources(&mut item.changed_by, &approval.source_refs);
                }
            }
            state
                .conflicts
                .get_mut(conflict_id)
                .expect("validated conflict id exists")
                .resolved = true;
        }
        Event::GoalRevised { goal } => {
            ensure_started(state, "goal_revised")?;
            validate_goal(goal, false, "goal_revised")?;
            let current = state.goal.as_ref().expect("started history has a goal");
            let expected_revision =
                current
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| HistoryError::InvalidTransition {
                        event: "goal_revised",
                        reason: "goal revision exhausted".to_owned(),
                    })?;
            if goal.revision != expected_revision
                || goal.supersedes_revision != Some(current.revision)
            {
                return Err(HistoryError::InvalidTransition {
                    event: "goal_revised",
                    reason: "goal revision must monotonically supersede the current revision"
                        .to_owned(),
                });
            }
            if goal.text == current.text {
                return Err(HistoryError::InvalidTransition {
                    event: "goal_revised",
                    reason: "goal revision must change the goal text".to_owned(),
                });
            }
            state.goal = Some(goal.clone());
        }
        Event::EvidenceAttached {
            intent_id,
            evidence,
        } => {
            ensure_started(state, "evidence_attached")?;
            validate_evidence(evidence, "evidence_attached")?;
            if state
                .intents
                .values()
                .any(|item| item.evidence.iter().any(|prior| prior.id == evidence.id))
            {
                return Err(HistoryError::DuplicateId {
                    kind: "evidence",
                    id: evidence.id.to_string(),
                });
            }
            let item =
                state
                    .intents
                    .get_mut(intent_id)
                    .ok_or_else(|| HistoryError::UnknownIntent {
                        id: intent_id.clone(),
                    })?;
            item.evidence.push(evidence.clone());
            item.evidence_state = EvidenceState::Satisfied;
        }
        Event::EvidenceInvalidated {
            intent_id,
            evidence_id,
            source_refs,
            reason,
        } => {
            ensure_started(state, "evidence_invalidated")?;
            validate_source_refs(source_refs, "evidence_invalidated")?;
            non_empty(reason, "evidence invalidation reason")?;
            let item = state
                .intents
                .get(intent_id)
                .ok_or_else(|| HistoryError::UnknownIntent {
                    id: intent_id.clone(),
                })?;
            if !item
                .evidence
                .iter()
                .any(|evidence| evidence.id == *evidence_id)
            {
                return Err(HistoryError::UnknownEvidence {
                    id: evidence_id.clone(),
                });
            }
            if !state.evidence_invalidated.insert(evidence_id.clone()) {
                return Err(HistoryError::InvalidTransition {
                    event: "evidence_invalidated",
                    reason: "evidence is already invalidated".to_owned(),
                });
            }
            let item = state
                .intents
                .get_mut(intent_id)
                .expect("validated intent id exists");
            item.evidence_state = if item
                .evidence
                .iter()
                .any(|evidence| !state.evidence_invalidated.contains(&evidence.id))
            {
                EvidenceState::Satisfied
            } else {
                EvidenceState::Reopened
            };
        }
        Event::RunClosed {
            source_refs,
            approval,
        } => {
            ensure_started(state, "run_closed")?;
            validate_authority(source_refs, approval.as_ref(), "run_closed")?;
            if state.conflicts.values().any(|conflict| !conflict.resolved) {
                return Err(HistoryError::InvalidTransition {
                    event: "run_closed",
                    reason: "open conflicts block closure".to_owned(),
                });
            }
            if state.intents.values().any(|item| {
                item.is_active() && !matches!(item.evidence_state, EvidenceState::Satisfied)
            }) {
                return Err(HistoryError::InvalidTransition {
                    event: "run_closed",
                    reason: "every active intent requires satisfied evidence".to_owned(),
                });
            }
            state.closed = true;
        }
    }
    Ok(())
}

fn ensure_started(state: &ReplayState, event: &'static str) -> Result<(), HistoryError> {
    if state.goal.is_none() {
        Err(if event == "run_started" {
            HistoryError::RunStartedRequired
        } else {
            HistoryError::InvalidTransition {
                event,
                reason: "event requires a preceding run_started".to_owned(),
            }
        })
    } else {
        Ok(())
    }
}

fn validate_new_item(
    item: &IntentItem,
    approval: Option<&Approval>,
    event: &'static str,
) -> Result<(), HistoryError> {
    validate_intent_id(&item.id, "intent id")?;
    non_empty(&item.text, "intent text")?;
    validate_source_refs(&item.introduced_by, event)?;
    validate_authority(&item.introduced_by, approval, event)?;
    validate_source_refs(&item.changed_by, event).or_else(|error| {
        if item.changed_by.is_empty() {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    if !matches!(item.lifecycle, IntentLifecycle::Active)
        || !matches!(item.evidence_state, EvidenceState::Unresolved)
    {
        return Err(HistoryError::InvalidTransition {
            event,
            reason: "new intent must be active with unresolved evidence".to_owned(),
        });
    }
    if !item.evidence.is_empty() {
        return Err(HistoryError::InvalidTransition {
            event,
            reason: "new intent cannot contain pre-attached evidence".to_owned(),
        });
    }
    for superseded_id in &item.supersedes {
        validate_intent_id(superseded_id, "superseded intent id")?;
    }
    Ok(())
}

fn validate_intent_id(id: &IntentId, field: &'static str) -> Result<(), HistoryError> {
    non_empty(id.as_str(), field)
}

fn validate_goal(
    goal: &GoalRevision,
    initial: bool,
    event: &'static str,
) -> Result<(), HistoryError> {
    non_empty(&goal.text, "goal text")?;
    validate_authority(&goal.source_refs, goal.approval.as_ref(), event)?;
    if initial && goal.revision == 0 {
        return Err(HistoryError::InvalidTransition {
            event,
            reason: "goal revision must be positive".to_owned(),
        });
    }
    Ok(())
}

fn validate_conflict(
    conflict: &Conflict,
    approval: Option<&Approval>,
    event: &'static str,
) -> Result<(), HistoryError> {
    validate_intent_id(&IntentId::new(conflict.id.as_str()), "conflict id")?;
    validate_source_refs(&conflict.source_refs, event)?;
    validate_authority(&conflict.source_refs, approval, event)?;
    if conflict.intent_ids.is_empty() {
        return Err(HistoryError::InvalidTransition {
            event,
            reason: "conflict must affect at least one intent".to_owned(),
        });
    }
    let mut intent_ids = BTreeSet::new();
    for intent_id in &conflict.intent_ids {
        validate_intent_id(intent_id, "conflict intent id")?;
        if !intent_ids.insert(intent_id) {
            return Err(HistoryError::InvalidTransition {
                event,
                reason: "conflict intent ids must be unique".to_owned(),
            });
        }
    }
    if conflict.alternatives.len() < 2 {
        return Err(HistoryError::InvalidTransition {
            event,
            reason: "conflict requires at least two alternatives".to_owned(),
        });
    }
    let mut alternative_ids = BTreeSet::new();
    for alternative in &conflict.alternatives {
        non_empty(&alternative.id, "conflict alternative id")?;
        non_empty(&alternative.text, "conflict alternative text")?;
        validate_source_refs(&alternative.source_refs, event)?;
        if !alternative_ids.insert(&alternative.id) {
            return Err(HistoryError::InvalidTransition {
                event,
                reason: "conflict alternative ids must be unique".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_evidence(evidence: &EvidenceRef, event: &'static str) -> Result<(), HistoryError> {
    non_empty(evidence.id.as_str(), "evidence id")?;
    non_empty(&evidence.summary, "evidence summary")?;
    validate_source_refs(&evidence.source_refs, event)
}

fn validate_authority(
    source_refs: &[SourceRef],
    approval: Option<&Approval>,
    event: &'static str,
) -> Result<(), HistoryError> {
    validate_source_refs(source_refs, event)?;
    if source_refs
        .iter()
        .any(|source| matches!(source.role, SourceRole::User))
    {
        if let Some(approval) = approval {
            approval.validate(event)?;
        }
        return Ok(());
    }
    let Some(approval) = approval else {
        return Err(HistoryError::UnauthorizedSource { event });
    };
    approval.validate(event)
}

fn validate_source_refs(
    source_refs: &[SourceRef],
    event: &'static str,
) -> Result<(), HistoryError> {
    if source_refs.is_empty() {
        return Err(HistoryError::MissingSourceRefs { event });
    }
    for source in source_refs {
        source.validate()?;
    }
    Ok(())
}

fn non_empty(value: &str, field: &'static str) -> Result<(), HistoryError> {
    if value.trim().is_empty() {
        Err(HistoryError::InvalidSourceRef { field })
    } else {
        Ok(())
    }
}

fn append_sources(target: &mut Vec<SourceRef>, sources: &[SourceRef]) {
    for source in sources {
        if !target.contains(source) {
            target.push(source.clone());
        }
    }
}

// Convenient names used by callers that prefer the architecture vocabulary.
pub type HistoryRecord = EventRecord;
pub type VersionedEvent = EventRecord;
pub type IntentEvent = Event;
pub type Lifecycle = IntentLifecycle;
