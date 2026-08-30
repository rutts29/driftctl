//! Same-session reconciliation and semantic commit controller.

use crate::codex_source::{self, SessionSelection};
use crate::enrollment::Enrollment;
use crate::goal_change_store::{GoalChangeStore, PendingGoalChange};
use crate::hook_event_store::HookEventStore;
use crate::inspect_state::InspectSource;
use crate::intent_history::{SourceProvider, SourceRole};
use crate::projection::{ActiveProjection, ProjectionConfig};
use crate::run_store::{RecoveredRun, RunStore, SourceCursor, SourceCursorComparison};
use crate::semantic_resolver::{self, InspectResolution};
use crate::session_bundle::{BundleRecord, NativeGoal, NeutralSessionBundle};

pub(crate) enum KeeperDecision {
    Ready(ActiveProjection),
    Blocked {
        projection: ActiveProjection,
        reason: String,
    },
}

impl KeeperDecision {
    pub(crate) fn projection(&self) -> &ActiveProjection {
        match self {
            Self::Ready(projection) | Self::Blocked { projection, .. } => projection,
        }
    }
}

struct LockedState {
    store: RunStore,
    recovered: RecoveredRun,
    native_goal_mismatch: bool,
}

pub(crate) fn on_session_start(enrollment: &Enrollment) -> Result<KeeperDecision, String> {
    decision_from_state(reconcile_provider(enrollment)?)
}

pub(crate) fn on_observation(enrollment: &Enrollment) -> Result<KeeperDecision, String> {
    decision_from_state(reconcile_provider(enrollment)?)
}

pub(crate) fn on_user_prompt(
    enrollment: &Enrollment,
    turn_id: &str,
    prompt: &str,
) -> Result<KeeperDecision, String> {
    let record_id = prompt_record_id(turn_id)?;
    let record = BundleRecord::new(&record_id, SourceRole::User, prompt)
        .map_err(|error| format!("invalid user prompt for keeper: {error}"))?;
    let receipt_digest = HookEventStore::prompt_digest(record.content());

    let initial = open_state(enrollment)?;
    let receipts = HookEventStore::open(&initial.store).map_err(|error| error.to_string())?;
    if receipts
        .matches(&record_id, &receipt_digest)
        .map_err(|error| error.to_string())?
    {
        return decision_from_state(initial);
    }
    if receipts
        .conflicts(&record_id, &receipt_digest)
        .map_err(|error| error.to_string())?
    {
        return Ok(KeeperDecision::Blocked {
            projection: initial.recovered.projection,
            reason: "the same Codex turn ID was delivered with different prompt content".to_owned(),
        });
    }
    drop(initial);

    let mut state = reconcile_provider(enrollment)?;
    if let Some(reason) = blocker(&state)? {
        return Ok(KeeperDecision::Blocked {
            projection: state.recovered.projection,
            reason,
        });
    }
    let receipts = HookEventStore::open(&state.store).map_err(|error| error.to_string())?;
    if receipts
        .matches(&record_id, &receipt_digest)
        .map_err(|error| error.to_string())?
    {
        return decision_from_state(state);
    }
    if receipts
        .conflicts(&record_id, &receipt_digest)
        .map_err(|error| error.to_string())?
    {
        return Ok(KeeperDecision::Blocked {
            projection: state.recovered.projection,
            reason: "the observed Codex prompt conflicts with the hook delivery".to_owned(),
        });
    }
    let cursor = state
        .recovered
        .source_cursor
        .as_ref()
        .ok_or_else(|| "attached run has no accepted source cursor".to_owned())?;

    let delta = NeutralSessionBundle::from_records_with_native_goal(
        SourceProvider::Codex,
        enrollment.session_id(),
        cursor.repository_digest(),
        NativeGoal::Unknown,
        vec![record.clone()],
    )
    .map_err(|error| format!("could not construct prompt delta: {error}"))?;
    let config = enrollment
        .keeper_config()
        .map_err(|error| error.to_string())?;
    let resolution = semantic_resolver::resolve_incremental(
        enrollment.repository(),
        &state.recovered.history,
        &state.recovered.projection,
        &delta,
        config,
        ProjectionConfig::new(state.recovered.projection.overflow.budget),
    )
    .map_err(|error| format!("keeper could not validate this steering: {error}"))?;
    let provider_cursor = cursor.clone();
    commit_resolution(&mut state, resolution, &provider_cursor)?;
    HookEventStore::open(&state.store)
        .and_then(|store| {
            store.record(
                &record_id,
                &receipt_digest,
                state.recovered.projection.revision,
            )
        })
        .map_err(|error| error.to_string())?;
    decision_from_state(state)
}

fn reconcile_provider(enrollment: &Enrollment) -> Result<LockedState, String> {
    let imported = codex_source::inspect(
        enrollment.repository(),
        SessionSelection::Explicit {
            id: enrollment.session_id(),
            allow_ancestor_cwd: enrollment.allow_ancestor_cwd(),
        },
    )
    .map_err(|error| error.to_string())?;
    let bundle = imported
        .neutral_bundle()
        .map_err(|error| error.to_string())?;
    let source = InspectSource::from_bundle(&bundle).map_err(|error| error.to_string())?;
    if source.run_id().as_str() != enrollment.run_id() {
        return Err("observed Codex session does not match its enrolled run".to_owned());
    }
    let mut state = open_state(enrollment)?;
    let accepted = state
        .recovered
        .source_cursor
        .as_ref()
        .ok_or_else(|| "attached run has no accepted source cursor".to_owned())?;
    match accepted
        .compare(source.cursor())
        .map_err(|error| error.to_string())?
    {
        SourceCursorComparison::Current => {}
        SourceCursorComparison::NewRecords(records) => {
            let accepted_count = source.cursor().accepted_record_count() - records.len();
            let receipts = HookEventStore::open(&state.store).map_err(|error| error.to_string())?;
            let mut unseen = Vec::new();
            for record in &bundle.records()[accepted_count..] {
                if matches!(record.role(), SourceRole::User)
                    && receipts
                        .claim_provider_record(
                            record.id(),
                            &HookEventStore::prompt_digest(record.content()),
                        )
                        .map_err(|error| error.to_string())?
                {
                    continue;
                }
                unseen.push(record.clone());
            }
            let has_unseen_authority = unseen
                .iter()
                .any(|record| matches!(record.role(), SourceRole::User));
            if !has_unseen_authority {
                codex_source::verify_unchanged(enrollment.repository(), &imported)
                    .map_err(|error| error.to_string())?;
                state
                    .store
                    .commit_projection_with_source_cursor(
                        &state.recovered.projection,
                        source.cursor(),
                    )
                    .map_err(|error| error.to_string())?;
                state.recovered.source_cursor = Some(source.cursor().clone());
            } else {
                let delta = NeutralSessionBundle::from_records_with_native_goal(
                    bundle.source().provider(),
                    bundle.source().session_ref_private(),
                    bundle.source().repository_digest(),
                    bundle.native_goal().clone(),
                    unseen,
                )
                .map_err(|error| error.to_string())?;
                let config = enrollment
                    .keeper_config()
                    .map_err(|error| error.to_string())?;
                let resolution = semantic_resolver::resolve_incremental(
                    enrollment.repository(),
                    &state.recovered.history,
                    &state.recovered.projection,
                    &delta,
                    config,
                    ProjectionConfig::new(state.recovered.projection.overflow.budget),
                )
                .map_err(|error| format!("keeper could not reconcile missed steering: {error}"))?;
                codex_source::verify_unchanged(enrollment.repository(), &imported)
                    .map_err(|error| error.to_string())?;
                commit_resolution(&mut state, resolution, source.cursor())?;
            }
        }
        SourceCursorComparison::Stale { .. } => {
            return Err("Codex source is behind the accepted session ledger".to_owned());
        }
        SourceCursorComparison::Rewrite { .. } => {
            return Err("accepted Codex source history was rewritten".to_owned());
        }
        SourceCursorComparison::SessionMismatch => {
            return Err("observed Codex source belongs to a different session".to_owned());
        }
    }
    state.native_goal_mismatch = bundle
        .native_goal()
        .text()
        .is_some_and(|goal| goal != state.recovered.projection.goal.text);
    Ok(state)
}

fn commit_resolution(
    state: &mut LockedState,
    resolution: InspectResolution,
    cursor: &SourceCursor,
) -> Result<(), String> {
    let accepted_events = state.recovered.history.records().len();
    for record in &resolution.history.records()[accepted_events..] {
        state
            .store
            .append_pending(record.clone())
            .map_err(|error| error.to_string())?;
    }
    if let Some(goal_change) = &resolution.goal_change {
        let proposal = PendingGoalChange::new(goal_change, cursor.digest())
            .map_err(|error| error.to_string())?;
        GoalChangeStore::open(&state.store)
            .and_then(|store| store.persist(&proposal))
            .map_err(|error| error.to_string())?;
    }
    state
        .store
        .commit_projection_with_source_cursor(&resolution.projection, cursor)
        .map_err(|error| error.to_string())?;
    state.recovered.history = resolution.history;
    state.recovered.projection = resolution.projection;
    state.recovered.pending_delta.clear();
    state.recovered.source_cursor = Some(cursor.clone());
    Ok(())
}

fn open_state(enrollment: &Enrollment) -> Result<LockedState, String> {
    let store = RunStore::open_default(enrollment.repository(), enrollment.run_id())
        .map_err(|error| error.to_string())?;
    let recovered = store.recover().map_err(|error| error.to_string())?;
    Ok(LockedState {
        store,
        recovered,
        native_goal_mismatch: false,
    })
}

fn decision_from_state(state: LockedState) -> Result<KeeperDecision, String> {
    if let Some(reason) = blocker(&state)? {
        Ok(KeeperDecision::Blocked {
            projection: state.recovered.projection,
            reason,
        })
    } else {
        Ok(KeeperDecision::Ready(state.recovered.projection))
    }
}

fn blocker(state: &LockedState) -> Result<Option<String>, String> {
    if state.native_goal_mismatch {
        return Ok(Some(
            "the Codex native goal differs from the accepted Driftctl goal and requires operator approval"
                .to_owned(),
        ));
    }
    if state.recovered.projection.continuation_blocked() {
        return Ok(Some(
            "active intent contains an unresolved conflict or exceeded its projection budget"
                .to_owned(),
        ));
    }
    let proposal_store = GoalChangeStore::open(&state.store).map_err(|error| error.to_string())?;
    if proposal_store
        .load_pending()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Ok(Some(
            "an overall goal change requires an explicit operator decision".to_owned(),
        ));
    }
    Ok(None)
}

fn prompt_record_id(turn_id: &str) -> Result<String, String> {
    if turn_id.trim().is_empty()
        || turn_id.len() > 16 * 1024
        || turn_id.chars().any(char::is_control)
    {
        return Err("Codex UserPromptSubmit requires a valid turn_id".to_owned());
    }
    Ok(format!("{turn_id}:0"))
}
