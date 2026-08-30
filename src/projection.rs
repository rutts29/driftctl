//! Deterministic, bounded active-intent projections.
//!
//! This module folds the already validated [`crate::intent_history::History`]
//! state into the prompt-facing projection.  It deliberately does not call a
//! model or mutate history: the source history remains the audit record, and
//! this module only derives a new value from it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::intent_history::{
    Conflict, Event, EvidenceState, GoalRevision, History, HistoryError, IntentItem, IntentKind,
    IntentLifecycle, SourceRef,
};

/// The schema version of an active projection payload.
pub const PROJECTION_SCHEMA_VERSION: u32 = 1;

/// The conservative default used by callers that do not choose a budget.
pub const DEFAULT_PROJECTION_BYTE_BUDGET: usize = 16 * 1024;

/// Configuration for deterministic projection rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionConfig {
    /// Maximum number of bytes in the serialized prompt payload.
    pub byte_budget: usize,
}

impl ProjectionConfig {
    #[must_use]
    pub const fn new(byte_budget: usize) -> Self {
        Self { byte_budget }
    }
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        Self::new(DEFAULT_PROJECTION_BYTE_BUDGET)
    }
}

impl From<usize> for ProjectionConfig {
    fn from(byte_budget: usize) -> Self {
        Self::new(byte_budget)
    }
}

/// The source position represented by a projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionSourceHead {
    /// The latest immutable history event sequence, represented as a stable
    /// local source-record label.
    pub record: String,
    /// The latest event sequence included in the projection.
    pub pending_sequence: u64,
}

/// Static provenance for the deterministic projection builder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionGeneratedBy {
    pub component: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub prompt_schema_version: u32,
}

/// The reason a closure attempt remains blocked.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureBlockerKind {
    UnresolvedRequirement,
    Conflict,
    ProjectionOverflow,
}

/// A deterministic, source-linked closure blocker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClosureBlocker {
    pub kind: ClosureBlockerKind,
    /// Intent or conflict identifier, when the blocker has one.
    pub id: Option<String>,
    pub reason: String,
    pub source_refs: Vec<SourceRef>,
}

/// Explicit closure state derived from the active projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClosureState {
    pub blockers: Vec<ClosureBlocker>,
}

impl ClosureState {
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }

    #[must_use]
    pub fn can_close(&self) -> bool {
        !self.is_blocked()
    }
}

/// Byte-accounting for the rendered prompt payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionOverflow {
    /// Exact UTF-8 byte length of the serialized prompt payload.
    pub bytes: usize,
    pub budget: usize,
    /// Number of intent/conflict items represented by that payload.
    pub item_count: usize,
    /// `None` means the payload is within budget; `Some` is a blocking reason.
    pub reason: Option<String>,
}

impl ProjectionOverflow {
    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.reason.is_some()
    }
}

/// The bounded active intent projection consumed by a continuation prompt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveProjection {
    pub schema_version: u32,
    pub revision: u64,
    pub source_head: ProjectionSourceHead,
    pub goal: GoalRevision,
    pub preserve: Vec<IntentItem>,
    pub frontier: Vec<IntentItem>,
    pub validation: Vec<IntentItem>,
    pub conflicts: Vec<Conflict>,
    pub closure: ClosureState,
    pub overflow: ProjectionOverflow,
    pub generated_by: ProjectionGeneratedBy,
}

impl ActiveProjection {
    /// Build a projection with either a [`ProjectionConfig`] or a byte budget.
    pub fn from_history<C>(history: &History, config: C) -> Result<Self, ProjectionError>
    where
        C: Into<ProjectionConfig>,
    {
        project(history, config)
    }

    /// Serialize exactly the prompt payload whose bytes are budgeted.
    pub fn try_rendered_prompt(&self) -> Result<String, ProjectionError> {
        serde_json::to_string(&PromptPayload {
            schema_version: self.schema_version,
            goal: &self.goal,
            preserve: &self.preserve,
            frontier: &self.frontier,
            validation: &self.validation,
            conflicts: &self.conflicts,
        })
        .map_err(ProjectionError::from)
    }

    /// Serialize exactly the prompt payload whose bytes are budgeted.
    #[must_use]
    pub fn rendered_prompt(&self) -> String {
        self.try_rendered_prompt()
            .expect("active projection prompt serialization must be infallible")
    }

    /// Return the exact UTF-8 byte length of [`Self::rendered_prompt`].
    #[must_use]
    pub fn rendered_bytes(&self) -> usize {
        self.rendered_prompt().len()
    }

    #[must_use]
    pub fn is_overflowed(&self) -> bool {
        self.overflow.is_overflow()
    }

    #[must_use]
    pub fn continuation_blocked(&self) -> bool {
        self.closure.is_blocked()
    }
}

/// A failure to account for history state in a projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionError {
    MissingAccountability {
        context: String,
    },
    InvalidState {
        reason: String,
    },
    InvalidSource {
        context: String,
        source: HistoryError,
    },
    Serialization(String),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAccountability { context } => {
                write!(formatter, "projection accountability is missing: {context}")
            }
            Self::InvalidState { reason } => {
                write!(formatter, "invalid projection state: {reason}")
            }
            Self::InvalidSource { context, source } => {
                write!(
                    formatter,
                    "invalid projection source for {context}: {source}"
                )
            }
            Self::Serialization(message) => {
                write!(formatter, "projection serialization failed: {message}")
            }
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<serde_json::Error> for ProjectionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

impl From<HistoryError> for ProjectionError {
    fn from(error: HistoryError) -> Self {
        Self::InvalidSource {
            context: "history".to_owned(),
            source: error,
        }
    }
}

#[derive(Serialize)]
struct PromptPayload<'a> {
    schema_version: u32,
    goal: &'a GoalRevision,
    preserve: &'a [IntentItem],
    frontier: &'a [IntentItem],
    validation: &'a [IntentItem],
    conflicts: &'a [Conflict],
}

/// Fold a validated history into a deterministic active projection.
pub fn project<C>(history: &History, config: C) -> Result<ActiveProjection, ProjectionError>
where
    C: Into<ProjectionConfig>,
{
    let config = config.into();
    let snapshot = history.snapshot();
    validate_goal(&snapshot.goal)?;

    for item in snapshot.intents.values() {
        validate_item(item)?;
    }
    validate_supersession_edges(&snapshot.intents)?;

    let mut preserve = Vec::new();
    let mut frontier = Vec::new();
    let mut validation = Vec::new();
    for item in snapshot.intents.values().filter(|item| item.is_active()) {
        match item.kind {
            IntentKind::Validation | IntentKind::StopCondition => validation.push(item.clone()),
            _ if matches!(item.evidence_state, EvidenceState::Satisfied) => {
                preserve.push(item.clone());
            }
            _ => frontier.push(item.clone()),
        }
    }

    let mut conflicts = Vec::new();
    for conflict in snapshot.open_conflicts.values() {
        validate_conflict(conflict, &snapshot.intents)?;
        conflicts.push(stabilize_conflict(conflict));
    }

    let mut blockers = Vec::new();
    for item in &frontier {
        blockers.push(ClosureBlocker {
            kind: ClosureBlockerKind::UnresolvedRequirement,
            id: Some(item.id.to_string()),
            reason: "active intent has unresolved or reopened evidence".to_owned(),
            source_refs: item.introduced_by.clone(),
        });
    }
    for item in &validation {
        if !matches!(item.evidence_state, EvidenceState::Satisfied) {
            blockers.push(ClosureBlocker {
                kind: ClosureBlockerKind::UnresolvedRequirement,
                id: Some(item.id.to_string()),
                reason: "validation obligation has unresolved or reopened evidence".to_owned(),
                source_refs: item.introduced_by.clone(),
            });
        }
    }
    for conflict in &conflicts {
        blockers.push(ClosureBlocker {
            kind: ClosureBlockerKind::Conflict,
            id: Some(conflict.id.to_string()),
            reason: "unresolved semantic conflict blocks continuation".to_owned(),
            source_refs: conflict.source_refs.clone(),
        });
    }

    let item_count = preserve.len()
        + frontier.len()
        + validation.len()
        + conflicts
            .iter()
            .map(|conflict| conflict.intent_ids.len())
            .sum::<usize>();
    let source_sequence = history
        .records()
        .last()
        .map(|record| record.sequence)
        .ok_or_else(|| ProjectionError::InvalidState {
            reason: "history has no source records".to_owned(),
        })?;
    let mut projection = ActiveProjection {
        schema_version: PROJECTION_SCHEMA_VERSION,
        revision: source_sequence,
        source_head: ProjectionSourceHead {
            record: latest_source_record(history).ok_or_else(|| {
                ProjectionError::MissingAccountability {
                    context: "history source head".to_owned(),
                }
            })?,
            pending_sequence: source_sequence,
        },
        goal: snapshot.goal,
        preserve,
        frontier,
        validation,
        conflicts,
        closure: ClosureState { blockers },
        overflow: ProjectionOverflow {
            bytes: 0,
            budget: config.byte_budget,
            item_count,
            reason: None,
        },
        generated_by: ProjectionGeneratedBy {
            component: "driftctl.projection".to_owned(),
            model: None,
            reasoning: None,
            prompt_schema_version: PROJECTION_SCHEMA_VERSION,
        },
    };

    let bytes = projection.try_rendered_prompt()?.len();
    projection.overflow.bytes = bytes;
    if bytes > config.byte_budget {
        projection.overflow.reason = Some(format!(
            "rendered prompt payload is {bytes} bytes, over budget {}",
            config.byte_budget
        ));
        projection.closure.blockers.push(ClosureBlocker {
            kind: ClosureBlockerKind::ProjectionOverflow,
            id: None,
            reason: projection
                .overflow
                .reason
                .clone()
                .expect("overflow reason was set"),
            source_refs: Vec::new(),
        });
    }

    Ok(projection)
}

fn validate_goal(goal: &GoalRevision) -> Result<(), ProjectionError> {
    if goal.text.trim().is_empty() {
        return Err(ProjectionError::MissingAccountability {
            context: "goal text".to_owned(),
        });
    }
    require_sources(&goal.source_refs, "goal")?;
    if let Some(approval) = &goal.approval {
        require_sources(&approval.source_refs, "goal approval")?;
    }
    Ok(())
}

fn validate_item(item: &IntentItem) -> Result<(), ProjectionError> {
    require_sources(&item.introduced_by, &format!("intent {}", item.id))?;
    if !item.is_active() && item.changed_by.is_empty() {
        return Err(ProjectionError::MissingAccountability {
            context: format!("lifecycle transition for intent {}", item.id),
        });
    }
    if !item.supersedes.is_empty() && item.changed_by.is_empty() {
        return Err(ProjectionError::MissingAccountability {
            context: format!("supersession for intent {}", item.id),
        });
    }
    validate_sources(&item.changed_by, &format!("intent {} changes", item.id))?;
    for evidence in &item.evidence {
        if evidence.summary.trim().is_empty() {
            return Err(ProjectionError::MissingAccountability {
                context: format!("evidence {} summary", evidence.id),
            });
        }
        require_sources(
            &evidence.source_refs,
            &format!("evidence {} for intent {}", evidence.id, item.id),
        )?;
    }
    if matches!(item.evidence_state, EvidenceState::Satisfied) && item.evidence.is_empty() {
        return Err(ProjectionError::InvalidState {
            reason: format!("satisfied intent {} has no evidence", item.id),
        });
    }
    Ok(())
}

fn validate_supersession_edges(
    intents: &std::collections::BTreeMap<crate::intent_history::IntentId, IntentItem>,
) -> Result<(), ProjectionError> {
    for item in intents.values() {
        for previous_id in &item.supersedes {
            let Some(previous) = intents.get(previous_id) else {
                return Err(ProjectionError::InvalidState {
                    reason: format!(
                        "intent {} supersedes missing intent {}",
                        item.id, previous_id
                    ),
                });
            };
            if previous.is_active() {
                return Err(ProjectionError::InvalidState {
                    reason: format!(
                        "intent {} supersedes active intent {}",
                        item.id, previous_id
                    ),
                });
            }
        }
    }
    Ok(())
}

fn validate_conflict(
    conflict: &Conflict,
    intents: &std::collections::BTreeMap<crate::intent_history::IntentId, IntentItem>,
) -> Result<(), ProjectionError> {
    require_sources(&conflict.source_refs, &format!("conflict {}", conflict.id))?;
    for alternative in &conflict.alternatives {
        require_sources(
            &alternative.source_refs,
            &format!("alternative {} in conflict {}", alternative.id, conflict.id),
        )?;
    }
    for intent_id in &conflict.intent_ids {
        let Some(item) = intents.get(intent_id) else {
            return Err(ProjectionError::InvalidState {
                reason: format!(
                    "conflict {} references missing intent {}",
                    conflict.id, intent_id
                ),
            });
        };
        if !matches!(item.lifecycle, IntentLifecycle::Conflicted) {
            return Err(ProjectionError::InvalidState {
                reason: format!(
                    "open conflict {} references non-conflicted intent {}",
                    conflict.id, intent_id
                ),
            });
        }
    }
    Ok(())
}

fn stabilize_conflict(conflict: &Conflict) -> Conflict {
    let mut conflict = conflict.clone();
    conflict.intent_ids.sort();
    conflict
        .alternatives
        .sort_by(|left, right| left.id.cmp(&right.id));
    conflict
}

fn require_sources(sources: &[SourceRef], context: &str) -> Result<(), ProjectionError> {
    if sources.is_empty() {
        return Err(ProjectionError::MissingAccountability {
            context: context.to_owned(),
        });
    }
    validate_sources(sources, context)
}

fn validate_sources(sources: &[SourceRef], context: &str) -> Result<(), ProjectionError> {
    for source in sources {
        source
            .validate()
            .map_err(|error| ProjectionError::InvalidSource {
                context: context.to_owned(),
                source: error,
            })?;
    }
    Ok(())
}

fn latest_source_record(history: &History) -> Option<String> {
    history
        .records()
        .iter()
        .rev()
        .find_map(|record| event_source_record(&record.event).map(str::to_owned))
}

fn event_source_record(event: &Event) -> Option<&str> {
    let source = match event {
        Event::RunStarted { goal } | Event::GoalRevised { goal } => goal.source_refs.first(),
        Event::RequirementAdded { item, .. }
        | Event::RequirementSuperseded {
            replacement: item, ..
        } => item.introduced_by.first(),
        Event::RequirementWithdrawn { source_refs, .. }
        | Event::EvidenceInvalidated { source_refs, .. }
        | Event::RunClosed { source_refs, .. } => source_refs.first(),
        Event::ConflictRaised { conflict, .. } => conflict.source_refs.first(),
        Event::ConflictResolved { resolution, .. } => resolution.source_refs.first(),
        Event::EvidenceAttached { evidence, .. } => evidence.source_refs.first(),
    }?;
    Some(source.record.as_str())
}
