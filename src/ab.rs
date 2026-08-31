//! Private state and execution boundaries for prospective paired A/B runs.

use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::codex_child::{CodexChildAdapter, GoalObservation, PreservedForkRequest, WorkerPolicy};
use crate::codex_source::{self, SessionSelection};
use crate::inspect_state::InspectSource;
use crate::run_store::{RunStore, SourceCursorComparison};
use crate::workspace::ResolvedSourceRef;

const SCHEMA_VERSION: u32 = 1;
const EXPERIMENT_KIND: &str = "prospective_paired";
static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct AbFailure {
    message: String,
    blocked: bool,
}

impl AbFailure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            blocked: false,
        }
    }

    fn blocked(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            blocked: true,
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn is_blocked(&self) -> bool {
        self.blocked
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExperimentStatus {
    Preparing,
    Ready,
    Reporting,
    Evaluated,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceState {
    repository: PathBuf,
    session_id: String,
    inspect_run_id: String,
    source_head: String,
    source_digest: String,
    record_count: usize,
    authoritative_user_record_count: usize,
    allow_ancestor_cwd: bool,
    source_attestation_digest: Option<String>,
    #[serde(default)]
    checkpoint_kind: Option<String>,
    #[serde(default)]
    through_turn_id: Option<String>,
    #[serde(default)]
    source_commit: Option<String>,
    #[serde(default)]
    observed_source_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StartingState {
    workspace_manifest_digest: String,
    candidate_digest: String,
    inherited_goal_state: String,
    inherited_goal_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArmState {
    session_id: String,
    cwd: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerPolicyState {
    model: String,
    effort: String,
    sandbox: String,
    approval_policy: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArmCounts {
    post_checkpoint_records: usize,
    post_checkpoint_user_prompts: usize,
    explicit_on_recorded: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportProgress {
    command_digest: String,
    baseline_counts: ArmCounts,
    workflow_counts: ArmCounts,
    baseline_verification: Option<crate::verification::VerificationResult>,
    workflow_verification: Option<crate::verification::VerificationResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExperimentState {
    schema_version: u32,
    experiment_kind: String,
    run_id: String,
    status: ExperimentStatus,
    created_at_unix_ms: u128,
    source: SourceState,
    starting: Option<StartingState>,
    worker_policy: WorkerPolicyState,
    baseline: Option<ArmState>,
    workflow: Option<ArmState>,
    failure: Option<String>,
    report_progress: Option<ReportProgress>,
    report: Option<Value>,
}

pub(crate) struct AbOutput {
    pub(crate) document: Value,
    pub(crate) text: String,
}

pub(crate) struct HistoricalCheckpoint {
    through_turn_id: String,
    source_ref: ResolvedSourceRef,
}

impl HistoricalCheckpoint {
    pub(crate) fn new(through_turn_id: String, source_ref: ResolvedSourceRef) -> Self {
        Self {
            through_turn_id,
            source_ref,
        }
    }
}

pub(crate) fn prepare(
    root: &Path,
    selection: SessionSelection<'_>,
    inspect_run_id: &str,
    historical: Option<HistoricalCheckpoint>,
) -> Result<AbOutput, AbFailure> {
    let allow_ancestor_cwd = matches!(
        &selection,
        SessionSelection::Explicit {
            allow_ancestor_cwd: true,
            ..
        }
    );
    let imported = match historical.as_ref() {
        Some(checkpoint) => {
            codex_source::inspect_through_turn(root, selection, &checkpoint.through_turn_id)
        }
        None => codex_source::inspect(root, selection),
    }
    .map_err(|error| {
        if error.is_blocked() {
            AbFailure::blocked(error.to_string())
        } else {
            AbFailure::error(error.to_string())
        }
    })?;
    let observed_source = if historical.is_some() {
        codex_source::inspect(
            root,
            SessionSelection::Explicit {
                id: imported.session_id(),
                allow_ancestor_cwd,
            },
        )
        .map_err(|error| AbFailure::error(error.to_string()))?
    } else {
        imported.clone()
    };
    let bundle = imported
        .neutral_bundle()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let source =
        InspectSource::from_bundle(&bundle).map_err(|error| AbFailure::error(error.to_string()))?;
    if source.run_id().as_str() != inspect_run_id {
        return Err(AbFailure::blocked(
            "source changed after checkpoint resolution; prepare again",
        ));
    }
    let inspect_store = RunStore::open_default(root, inspect_run_id)
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let recovered = inspect_store
        .recover()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let Some(accepted_cursor) = recovered.source_cursor.as_ref() else {
        return Err(AbFailure::error(
            "resolved source checkpoint has no accepted source cursor",
        ));
    };
    if !matches!(
        accepted_cursor.compare(source.cursor()),
        Ok(SourceCursorComparison::Current)
    ) {
        return Err(AbFailure::blocked(
            "source changed after checkpoint resolution; prepare again",
        ));
    }
    codex_source::verify_unchanged(root, &imported)
        .map_err(|error| AbFailure::blocked(error.to_string()))?;
    let source_session_id = bundle.source().session_ref_private();
    if crate::enrollment::load(source_session_id)
        .map_err(|error| AbFailure::error(error.to_string()))?
        .is_some()
    {
        return Err(AbFailure::blocked(
            "the source session is already enrolled; use a detached persisted midpoint for a controlled A/B run",
        ));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|error| AbFailure::error(format!("could not canonicalize source: {error}")))?;
    let source_attestation_digest = crate::workspace::source_attestation_digest(&canonical_root)
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let worker_policy = WorkerPolicy::luna_max();
    let state = ExperimentState {
        schema_version: SCHEMA_VERSION,
        experiment_kind: EXPERIMENT_KIND.to_owned(),
        run_id: String::new(),
        status: ExperimentStatus::Preparing,
        created_at_unix_ms: unix_ms()?,
        source: SourceState {
            repository: canonical_root.clone(),
            session_id: source_session_id.to_owned(),
            inspect_run_id: inspect_run_id.to_owned(),
            source_head: bundle.source().head().to_owned(),
            source_digest: bundle.source().digest().to_owned(),
            record_count: bundle.records().len(),
            authoritative_user_record_count: bundle.authoritative_records().len(),
            allow_ancestor_cwd,
            source_attestation_digest: Some(source_attestation_digest.clone()),
            checkpoint_kind: Some(if historical.is_some() {
                "historical_turn".to_owned()
            } else {
                "full_session".to_owned()
            }),
            through_turn_id: historical
                .as_ref()
                .map(|checkpoint| checkpoint.through_turn_id.clone()),
            source_commit: historical
                .as_ref()
                .map(|checkpoint| checkpoint.source_ref.commit().to_owned()),
            observed_source_digest: Some(observed_source.source_digest()),
        },
        starting: None,
        worker_policy: WorkerPolicyState {
            model: worker_policy.model().to_owned(),
            effort: worker_policy.effort().to_owned(),
            sandbox: "workspace_write".to_owned(),
            approval_policy: "never".to_owned(),
        },
        baseline: None,
        workflow: None,
        failure: None,
        report_progress: None,
        report: None,
    };
    let mut store = ExperimentStore::create(state)?;
    let workspaces = store.path.join("workspaces");
    crate::run_store::ensure_private_directory(&workspaces)
        .map_err(|error| store.blocked(error.to_string()))?;
    let pair = match historical.as_ref() {
        Some(checkpoint) => crate::workspace::isolate_workspace_at_ref(
            &canonical_root,
            &workspaces,
            &checkpoint.source_ref,
        ),
        None => crate::workspace::isolate_workspace(&canonical_root, &workspaces),
    }
    .map_err(|error| store.blocked(error.to_string()))?;
    if pair.baseline().manifest() != pair.workflow().manifest() {
        return Err(store.blocked("candidate starting manifests are not equal"));
    }
    let baseline_digest = crate::verification::candidate_digest(pair.baseline().root())
        .map_err(|error| store.blocked(error.to_string()))?;
    let workflow_digest = crate::verification::candidate_digest(pair.workflow().root())
        .map_err(|error| store.blocked(error.to_string()))?;
    if baseline_digest != workflow_digest {
        return Err(store.blocked("candidate starting digests are not equal"));
    }
    store.state.starting = Some(StartingState {
        workspace_manifest_digest: pair.baseline().manifest().digest().to_owned(),
        candidate_digest: baseline_digest,
        inherited_goal_state: "pending".to_owned(),
        inherited_goal_digest: None,
    });
    store.persist()?;

    let adapter = CodexChildAdapter::from_environment();
    let mut baseline_request = PreservedForkRequest::new(source_session_id, pair.baseline().root())
        .map_err(|error| store.blocked(error.to_string()))?
        .with_worker_policy(worker_policy.clone());
    if let Some(checkpoint) = historical.as_ref() {
        baseline_request = baseline_request
            .through_turn(&checkpoint.through_turn_id)
            .map_err(|error| store.blocked(error.to_string()))?;
    }
    let baseline = adapter
        .fork_preserving_goal(baseline_request)
        .map_err(|error| store.blocked(error.to_string()))?;
    store.state.baseline = Some(ArmState {
        session_id: baseline.child_id().to_owned(),
        cwd: baseline.child_cwd().to_owned(),
    });
    store.persist()?;

    let mut workflow_request = PreservedForkRequest::new(source_session_id, pair.workflow().root())
        .map_err(|error| store.blocked(error.to_string()))?
        .with_worker_policy(worker_policy);
    if let Some(checkpoint) = historical.as_ref() {
        workflow_request = workflow_request
            .through_turn(&checkpoint.through_turn_id)
            .map_err(|error| store.blocked(error.to_string()))?;
    }
    let workflow = adapter
        .fork_preserving_goal(workflow_request)
        .map_err(|error| store.blocked(error.to_string()))?;
    if workflow.child_id() == baseline.child_id() {
        return Err(store.blocked("provider returned the same child for both A/B arms"));
    }
    if baseline.parent_goal() != workflow.parent_goal()
        || baseline.child_goal() != workflow.child_goal()
    {
        return Err(store.blocked("A/B arms did not preserve the same inherited goal"));
    }
    let (goal_state, goal_digest) = goal_fingerprint(baseline.child_goal());
    let starting = store
        .state
        .starting
        .as_mut()
        .expect("starting state persisted before forks");
    starting.inherited_goal_state = goal_state;
    starting.inherited_goal_digest = goal_digest;
    store.state.workflow = Some(ArmState {
        session_id: workflow.child_id().to_owned(),
        cwd: workflow.child_cwd().to_owned(),
    });

    if historical.is_some() {
        for (session_id, cwd) in [
            (baseline.child_id(), baseline.child_cwd()),
            (workflow.child_id(), workflow.child_cwd()),
        ] {
            let child = codex_source::inspect(
                cwd,
                SessionSelection::Explicit {
                    id: session_id,
                    allow_ancestor_cwd: false,
                },
            )
            .map_err(|error| store.blocked(error.to_string()))?;
            let child_bundle = child
                .neutral_bundle()
                .map_err(|error| store.blocked(error.to_string()))?;
            if child_bundle.records() != bundle.records() {
                return Err(store.blocked(
                    "historical A/B child does not contain the exact selected source prefix",
                ));
            }
        }
    }

    for session_id in [baseline.child_id(), workflow.child_id()] {
        if crate::enrollment::load(session_id)
            .map_err(|error| store.blocked(error.to_string()))?
            .is_some()
        {
            return Err(store.blocked("prepare unexpectedly found an enrolled A/B child session"));
        }
    }
    codex_source::verify_unchanged(&canonical_root, &imported)
        .map_err(|error| store.blocked(error.to_string()))?;
    codex_source::verify_unchanged(&canonical_root, &observed_source)
        .map_err(|error| store.blocked(error.to_string()))?;
    let observed_attestation = crate::workspace::source_attestation_digest(&canonical_root)
        .map_err(|error| store.blocked(error.to_string()))?;
    if observed_attestation != source_attestation_digest {
        return Err(store.blocked("source repository changed during A/B prepare"));
    }
    store.state.status = ExperimentStatus::Ready;
    store.persist()?;
    Ok(prepare_output(&store.state))
}

pub(crate) fn report(run_id: &str, command: Vec<OsString>) -> Result<AbOutput, AbFailure> {
    if command.is_empty() || command.iter().any(|argument| argument.is_empty()) {
        return Err(AbFailure::error("ab report requires a verifier command"));
    }
    let command = canonicalize_verifier_command(command)?;
    let requested_command_digest = crate::verification::command_digest(&command);
    let mut store = ExperimentStore::open(run_id)?;
    if let Some(document) = store.state.report.clone() {
        let Some(progress) = store.state.report_progress.as_ref() else {
            return Err(AbFailure::error("evaluated A/B run has no report progress"));
        };
        if progress.command_digest != requested_command_digest {
            return Err(AbFailure::blocked(
                "this A/B run was already evaluated with a different verifier",
            ));
        }
        return Ok(report_output(with_cached(document, true), true));
    }
    if !matches!(
        store.state.status,
        ExperimentStatus::Ready | ExperimentStatus::Reporting
    ) {
        return Err(AbFailure::blocked(match store.state.status {
            ExperimentStatus::Blocked => "the A/B prepare run is blocked",
            ExperimentStatus::Preparing => "the A/B prepare run is incomplete",
            ExperimentStatus::Evaluated => "the A/B report state is incomplete",
            ExperimentStatus::Ready | ExperimentStatus::Reporting => unreachable!(),
        }));
    }
    let baseline = store
        .state
        .baseline
        .clone()
        .ok_or_else(|| AbFailure::blocked("A/B run has no baseline arm"))?;
    let workflow = store
        .state
        .workflow
        .clone()
        .ok_or_else(|| AbFailure::blocked("A/B run has no workflow arm"))?;
    let baseline_cwd = validate_arm_path(&store.path, &baseline)?;
    let workflow_cwd = validate_arm_path(&store.path, &workflow)?;
    reject_candidate_resolved_verifier_inputs(&command, &baseline_cwd, &workflow_cwd)?;

    if crate::enrollment::load(&baseline.session_id)
        .map_err(|error| AbFailure::error(error.to_string()))?
        .is_some()
    {
        return Err(AbFailure::blocked(
            "baseline is enrolled; detach it before reporting the controlled pair",
        ));
    }
    let workflow_enrollment = crate::enrollment::load(&workflow.session_id)
        .map_err(|error| AbFailure::error(error.to_string()))?
        .ok_or_else(|| {
            AbFailure::blocked(
                "workflow is not attached; resume that exact fork and run `$driftctl on`",
            )
        })?;
    if workflow_enrollment.repository() != workflow_cwd {
        return Err(AbFailure::blocked(
            "workflow enrollment does not bind the prepared workflow candidate",
        ));
    }
    let workflow_run = RunStore::open_default(
        workflow_enrollment.repository(),
        workflow_enrollment.run_id(),
    )
    .map_err(|error| AbFailure::error(error.to_string()))?;
    workflow_run
        .recover()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let keeper_usage = crate::keeper_metrics::load(&workflow_run).map_err(AbFailure::error)?;
    drop(workflow_run);

    let source_selection = SessionSelection::Explicit {
        id: &store.state.source.session_id,
        allow_ancestor_cwd: store.state.source.allow_ancestor_cwd,
    };
    let source_imported = match store.state.source.through_turn_id.as_deref() {
        Some(turn_id) => codex_source::inspect_through_turn(
            &store.state.source.repository,
            source_selection,
            turn_id,
        ),
        None => codex_source::inspect(&store.state.source.repository, source_selection),
    }
    .map_err(|error| AbFailure::blocked(error.to_string()))?;
    let observed_source = if store.state.source.through_turn_id.is_some() {
        codex_source::inspect(
            &store.state.source.repository,
            SessionSelection::Explicit {
                id: &store.state.source.session_id,
                allow_ancestor_cwd: store.state.source.allow_ancestor_cwd,
            },
        )
        .map_err(|error| AbFailure::blocked(error.to_string()))?
    } else {
        source_imported.clone()
    };
    let source_bundle = source_imported
        .neutral_bundle()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    if source_bundle.source().head() != store.state.source.source_head
        || source_bundle.source().digest() != store.state.source.source_digest
        || source_bundle.records().len() != store.state.source.record_count
        || source_bundle.authoritative_records().len()
            != store.state.source.authoritative_user_record_count
    {
        return Err(AbFailure::blocked(
            "source session changed after A/B prepare; this pair is no longer controlled",
        ));
    }
    let expected_observed_source_digest = store
        .state
        .source
        .observed_source_digest
        .as_deref()
        .unwrap_or(&store.state.source.source_digest);
    if observed_source.source_digest() != expected_observed_source_digest {
        return Err(AbFailure::blocked(
            "source session changed after A/B prepare; this pair is no longer controlled",
        ));
    }
    if crate::enrollment::load(&store.state.source.session_id)
        .map_err(|error| AbFailure::error(error.to_string()))?
        .is_some()
    {
        return Err(AbFailure::blocked(
            "source session became enrolled after A/B prepare",
        ));
    }
    let expected_attestation = store
        .state
        .source
        .source_attestation_digest
        .as_deref()
        .ok_or_else(|| AbFailure::error("A/B run has no source attestation"))?;
    let observed_attestation =
        crate::workspace::source_attestation_digest(&store.state.source.repository)
            .map_err(|error| AbFailure::blocked(error.to_string()))?;
    if observed_attestation != expected_attestation {
        return Err(AbFailure::blocked(
            "source repository changed after A/B prepare",
        ));
    }

    let baseline_imported = codex_source::inspect(
        &baseline_cwd,
        SessionSelection::Explicit {
            id: &baseline.session_id,
            allow_ancestor_cwd: false,
        },
    )
    .map_err(|error| AbFailure::blocked(error.to_string()))?;
    let workflow_imported = codex_source::inspect(
        &workflow_cwd,
        SessionSelection::Explicit {
            id: &workflow.session_id,
            allow_ancestor_cwd: false,
        },
    )
    .map_err(|error| AbFailure::blocked(error.to_string()))?;
    let baseline_bundle = baseline_imported
        .neutral_bundle()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let workflow_bundle = workflow_imported
        .neutral_bundle()
        .map_err(|error| AbFailure::error(error.to_string()))?;
    let baseline_counts = post_checkpoint_counts(&source_bundle, &baseline_bundle)?;
    let workflow_counts = post_checkpoint_counts(&source_bundle, &workflow_bundle)?;
    if baseline_counts.explicit_on_recorded {
        return Err(AbFailure::blocked(
            "baseline transcript contains `$driftctl on`; this pair is not a valid control",
        ));
    }
    if !workflow_counts.explicit_on_recorded {
        return Err(AbFailure::blocked(
            "workflow transcript does not contain the explicit `$driftctl on` activation",
        ));
    }
    codex_source::verify_unchanged(&store.state.source.repository, &source_imported)
        .map_err(|error| AbFailure::blocked(error.to_string()))?;
    codex_source::verify_unchanged(&store.state.source.repository, &observed_source)
        .map_err(|error| AbFailure::blocked(error.to_string()))?;
    codex_source::verify_unchanged(&baseline_cwd, &baseline_imported)
        .map_err(|error| AbFailure::blocked(error.to_string()))?;
    codex_source::verify_unchanged(&workflow_cwd, &workflow_imported)
        .map_err(|error| AbFailure::blocked(error.to_string()))?;

    match store.state.report_progress.as_ref() {
        Some(progress) if progress.command_digest != requested_command_digest => {
            return Err(AbFailure::blocked(
                "this A/B run already started with a different verifier",
            ));
        }
        Some(progress)
            if progress.baseline_counts != baseline_counts
                || progress.workflow_counts != workflow_counts =>
        {
            return Err(AbFailure::blocked(
                "an A/B session changed after report verification started",
            ));
        }
        Some(_) => {}
        None => {
            store.state.report_progress = Some(ReportProgress {
                command_digest: requested_command_digest.clone(),
                baseline_counts: baseline_counts.clone(),
                workflow_counts: workflow_counts.clone(),
                baseline_verification: None,
                workflow_verification: None,
            });
        }
    }
    store.state.status = ExperimentStatus::Reporting;
    store.persist()?;

    if store
        .state
        .report_progress
        .as_ref()
        .and_then(|progress| progress.baseline_verification.as_ref())
        .is_none()
    {
        let request = crate::verification::VerificationRequest::new(
            &baseline_cwd,
            "ab-paired-completion",
            command.iter().cloned(),
            store.path.join("artifacts/baseline"),
        )
        .map_err(|error| AbFailure::error(error.to_string()))?;
        let result = crate::verification::verify(&request)
            .map_err(|error| AbFailure::error(error.to_string()))?;
        store
            .state
            .report_progress
            .as_mut()
            .expect("report progress exists")
            .baseline_verification = Some(result);
        store.persist()?;
    }
    let baseline_verification = store
        .state
        .report_progress
        .as_ref()
        .and_then(|progress| progress.baseline_verification.as_ref())
        .expect("baseline verification persisted");
    if baseline_verification.status
        == crate::verification::VerificationStatus::ProtectedInputChanged
    {
        return Err(AbFailure::blocked(
            "the external A/B verifier changed during baseline verification",
        ));
    }
    let pinned_verifier_digest = baseline_verification.verifier_digest.clone();
    if store
        .state
        .report_progress
        .as_ref()
        .and_then(|progress| progress.workflow_verification.as_ref())
        .is_none()
    {
        let current_verifier_digest = crate::verification::verifier_digest(&workflow_cwd, &command)
            .map_err(|error| AbFailure::error(error.to_string()))?;
        if current_verifier_digest != pinned_verifier_digest {
            return Err(AbFailure::blocked(
                "the external A/B verifier changed between arm executions",
            ));
        }
        let request = crate::verification::VerificationRequest::new(
            &workflow_cwd,
            "ab-paired-completion",
            command.iter().cloned(),
            store.path.join("artifacts/workflow"),
        )
        .map_err(|error| AbFailure::error(error.to_string()))?;
        let result = crate::verification::verify(&request)
            .map_err(|error| AbFailure::error(error.to_string()))?;
        store
            .state
            .report_progress
            .as_mut()
            .expect("report progress exists")
            .workflow_verification = Some(result);
        store.persist()?;
    }
    let progress = store
        .state
        .report_progress
        .as_ref()
        .expect("completed report progress");
    let baseline_result = progress
        .baseline_verification
        .as_ref()
        .expect("baseline verification persisted");
    let workflow_result = progress
        .workflow_verification
        .as_ref()
        .expect("workflow verification persisted");
    if workflow_result.status == crate::verification::VerificationStatus::ProtectedInputChanged {
        return Err(AbFailure::blocked(
            "the external A/B verifier changed during workflow verification",
        ));
    }
    if baseline_result.verifier_digest != workflow_result.verifier_digest {
        return Err(AbFailure::blocked(
            "the A/B arms did not execute the same verifier content",
        ));
    }
    let document = json!({
        "schema_version":SCHEMA_VERSION,
        "experiment_kind":EXPERIMENT_KIND,
        "status":"evaluated",
        "run_id":store.state.run_id,
        "cached":false,
        "checkpoint":{
            "kind":store.state.source.checkpoint_kind.as_deref().unwrap_or("full_session"),
            "through_turn_id":store.state.source.through_turn_id,
            "source_commit":store.state.source.source_commit,
            "native_goal_basis":if store.state.source.through_turn_id.is_some() {"current_at_prepare"} else {"checkpoint"},
        },
        "source_unchanged":true,
        "fairness":{
            "starting_candidate_digest_equal":true,
            "inherited_goal_equal":true,
            "worker_policy_equal":true,
            "same_verifier_command":baseline_result.command_digest == workflow_result.command_digest
                && baseline_result.verifier_digest == workflow_result.verifier_digest,
            "verifier_execution_order":["baseline","workflow"],
        },
        "enrollment":{
            "baseline":"detached",
            "workflow":"attached_exact",
        },
        "baseline":arm_report(baseline_result, &progress.baseline_counts, None),
        "workflow":arm_report(
            workflow_result,
            &progress.workflow_counts,
            Some(json!({
                "status":"measured",
                "invocations":keeper_usage.invocations,
                "calls":keeper_usage.calls,
                "elapsed_ms":keeper_usage.elapsed_ms,
                "tokens":{
                    "input":keeper_usage.input_tokens,
                    "cached_input":keeper_usage.cached_input_tokens,
                    "output":keeper_usage.output_tokens,
                    "reasoning_output":keeper_usage.reasoning_output_tokens,
                },
            })),
        ),
        "comparison":{
            "primary_metric":"verified_completion",
            "outcome":comparison_outcome(baseline_result.passed(), workflow_result.passed()),
            "post_checkpoint_user_prompts_are_secondary":true,
        },
    });
    store.state.report = Some(document.clone());
    store.state.status = ExperimentStatus::Evaluated;
    store.persist()?;
    Ok(report_output(document, false))
}

fn canonicalize_verifier_command(mut command: Vec<OsString>) -> Result<Vec<OsString>, AbFailure> {
    let invocation_directory = env::current_dir()
        .map_err(|error| AbFailure::error(format!("read report directory: {error}")))?;
    let program = PathBuf::from(&command[0]);
    let resolved = if program.is_absolute() {
        program
    } else {
        invocation_directory.join(program)
    };
    let canonical = resolved.canonicalize().map_err(|_| {
        AbFailure::error(
            "A/B verifier program must resolve to one external regular file from the report invocation directory",
        )
    })?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|_| {
        AbFailure::error("A/B verifier program must resolve to one external regular file")
    })?;
    if !metadata.file_type().is_file() {
        return Err(AbFailure::error(
            "A/B verifier program must resolve to one external regular file",
        ));
    }
    command[0] = canonical.into_os_string();
    Ok(command)
}

fn reject_candidate_resolved_verifier_inputs(
    command: &[OsString],
    baseline: &Path,
    workflow: &Path,
) -> Result<(), AbFailure> {
    for (index, argument) in command.iter().enumerate() {
        let path = Path::new(argument);
        let resolves_inside_candidate = if path.is_absolute() {
            path.starts_with(baseline) || path.starts_with(workflow)
        } else {
            baseline.join(path).exists() || workflow.join(path).exists()
        };
        if resolves_inside_candidate {
            let kind = if index == 0 { "program" } else { "input" };
            return Err(AbFailure::error(format!(
                "A/B verifier {kind} must remain outside both candidate workspaces"
            )));
        }
    }
    Ok(())
}

fn arm_report(
    result: &crate::verification::VerificationResult,
    counts: &ArmCounts,
    keeper_overhead: Option<Value>,
) -> Value {
    let mut document = json!({
        "verified_completion":result.passed(),
        "verification_status":result.status.as_str(),
        "exit_code":result.exit_code,
        "elapsed_ms":result.elapsed_ms,
        "command_digest":result.command_digest,
        "verifier_digest":result.verifier_digest,
        "candidate_digest":result.candidate_after_digest,
        "artifact_id":result.artifact_id,
        "post_checkpoint_records":counts.post_checkpoint_records,
        "post_checkpoint_user_prompts":counts.post_checkpoint_user_prompts,
        "explicit_on_recorded":counts.explicit_on_recorded,
    });
    if let Some(keeper_overhead) = keeper_overhead {
        document
            .as_object_mut()
            .expect("arm report object")
            .insert("keeper_overhead".to_owned(), keeper_overhead);
    }
    document
}

fn comparison_outcome(baseline: bool, workflow: bool) -> &'static str {
    match (baseline, workflow) {
        (true, true) => "tie_pass",
        (true, false) => "baseline_only",
        (false, true) => "workflow_only",
        (false, false) => "tie_fail",
    }
}

fn post_checkpoint_counts(
    source: &crate::session_bundle::NeutralSessionBundle,
    arm: &crate::session_bundle::NeutralSessionBundle,
) -> Result<ArmCounts, AbFailure> {
    if arm.records().len() < source.records().len()
        || arm.records()[..source.records().len()] != *source.records()
    {
        return Err(AbFailure::blocked(
            "an A/B child does not preserve the exact prepared source prefix",
        ));
    }
    let tail = &arm.records()[source.records().len()..];
    Ok(ArmCounts {
        post_checkpoint_records: tail.len(),
        post_checkpoint_user_prompts: tail
            .iter()
            .filter(|record| record.is_authoritative())
            .count(),
        explicit_on_recorded: tail.iter().any(|record| {
            crate::plugin_control::parse(record.content())
                == Some(crate::plugin_control::PluginControl::On)
        }),
    })
}

fn validate_arm_path(store_path: &Path, arm: &ArmState) -> Result<PathBuf, AbFailure> {
    let workspace_root = store_path
        .join("workspaces")
        .canonicalize()
        .map_err(|error| AbFailure::error(format!("could not read A/B workspaces: {error}")))?;
    let cwd = arm
        .cwd
        .canonicalize()
        .map_err(|error| AbFailure::blocked(format!("A/B candidate is unavailable: {error}")))?;
    if cwd == workspace_root || !cwd.starts_with(&workspace_root) || cwd != arm.cwd {
        return Err(AbFailure::blocked(
            "A/B candidate path escaped its private experiment workspace",
        ));
    }
    Ok(cwd)
}

fn with_cached(mut document: Value, cached: bool) -> Value {
    if let Some(fields) = document.as_object_mut() {
        fields.insert("cached".to_owned(), json!(cached));
    }
    document
}

fn report_output(document: Value, cached: bool) -> AbOutput {
    let run_id = document["run_id"].as_str().unwrap_or("unknown");
    let baseline = document["baseline"]["verified_completion"]
        .as_bool()
        .unwrap_or(false);
    let workflow = document["workflow"]["verified_completion"]
        .as_bool()
        .unwrap_or(false);
    AbOutput {
        text: format!(
            "A/B run {run_id}: baseline verified={baseline}, workflow verified={workflow}{}",
            if cached { " (cached)" } else { "" },
        ),
        document,
    }
}

fn prepare_output(state: &ExperimentState) -> AbOutput {
    let baseline = state.baseline.as_ref().expect("ready baseline");
    let workflow = state.workflow.as_ref().expect("ready workflow");
    let document = json!({
        "schema_version":SCHEMA_VERSION,
        "experiment_kind":EXPERIMENT_KIND,
        "status":"ready",
        "run_id":state.run_id,
        "checkpoint":{
            "kind":state.source.checkpoint_kind.as_deref().unwrap_or("full_session"),
            "through_turn_id":state.source.through_turn_id,
            "source_commit":state.source.source_commit,
            "native_goal_basis":if state.source.through_turn_id.is_some() {"current_at_prepare"} else {"checkpoint"},
        },
        "fairness":{
            "starting_candidate_digest_equal":true,
            "inherited_goal_equal":true,
            "worker_policy_equal":true,
            "turns_started_by_prepare":0,
            "enrollments_created_by_prepare":0,
        },
        "baseline":{
            "session_id":baseline.session_id,
            "cwd":baseline.cwd,
            "resume_command":["codex","resume",baseline.session_id],
            "driftctl":"off",
        },
        "workflow":{
            "session_id":workflow.session_id,
            "cwd":workflow.cwd,
            "resume_command":["codex","resume",workflow.session_id],
            "activation":"$driftctl on",
            "driftctl":"off_until_explicit_activation",
        },
        "source_unchanged":true,
    });
    AbOutput {
        text: format!(
            "A/B run {} ready\nbaseline: cd {} && codex resume {}\nworkflow: cd {} && codex resume {}; then run `$driftctl on`",
            state.run_id,
            baseline.cwd.display(),
            baseline.session_id,
            workflow.cwd.display(),
            workflow.session_id,
        ),
        document,
    }
}

fn goal_fingerprint(goal: &GoalObservation) -> (String, Option<String>) {
    match goal {
        GoalObservation::Absent => ("absent".to_owned(), None),
        GoalObservation::Known(text) => (
            "known".to_owned(),
            Some(format!("sha256:{:x}", Sha256::digest(text.as_bytes()))),
        ),
    }
}

struct ExperimentStore {
    path: PathBuf,
    state: ExperimentState,
    _lock: File,
}

impl ExperimentStore {
    fn create(mut state: ExperimentState) -> Result<Self, AbFailure> {
        let root = RunStore::default_state_root()
            .map_err(|error| AbFailure::error(error.to_string()))?
            .join("ab");
        crate::run_store::ensure_private_directory(&root)
            .map_err(|error| AbFailure::error(error.to_string()))?;
        for _ in 0..1024 {
            let run_id = new_run_id()?;
            let path = root.join(&run_id);
            match create_private_directory(&path) {
                Ok(()) => {
                    state.run_id = run_id;
                    let lock = create_lock(&path)?;
                    let store = Self {
                        path,
                        state,
                        _lock: lock,
                    };
                    store.persist()?;
                    return Ok(store);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(AbFailure::error(format!(
                        "could not create A/B run directory: {error}"
                    )));
                }
            }
        }
        Err(AbFailure::error("could not allocate an A/B run ID"))
    }

    fn open(run_id: &str) -> Result<Self, AbFailure> {
        validate_run_id(run_id)?;
        let root = RunStore::default_state_root()
            .map_err(|error| AbFailure::error(error.to_string()))?
            .join("ab");
        ensure_private_existing_directory(&root)?;
        let path = root.join(run_id);
        ensure_private_existing_directory(&path)?;
        let lock = open_lock(&path)?;
        let state_path = path.join("experiment.json");
        ensure_private_file(&state_path)?;
        let bytes = fs::read(&state_path)
            .map_err(|error| AbFailure::error(format!("could not read A/B state: {error}")))?;
        let state: ExperimentState = serde_json::from_slice(&bytes)
            .map_err(|error| AbFailure::error(format!("invalid A/B state: {error}")))?;
        if state.schema_version != SCHEMA_VERSION
            || state.experiment_kind != EXPERIMENT_KIND
            || state.run_id != run_id
        {
            return Err(AbFailure::error(
                "A/B state does not match the requested run",
            ));
        }
        Ok(Self {
            path,
            state,
            _lock: lock,
        })
    }

    fn persist(&self) -> Result<(), AbFailure> {
        let bytes = serde_json::to_vec(&self.state)
            .map_err(|error| AbFailure::error(format!("could not encode A/B state: {error}")))?;
        write_atomic_private(&self.path.join("experiment.json"), &bytes)
    }

    fn blocked(&mut self, message: impl Into<String>) -> AbFailure {
        let message = message.into();
        self.state.status = ExperimentStatus::Blocked;
        self.state.failure = Some(message.clone());
        let _ = self.persist();
        AbFailure::blocked(format!("A/B run {} blocked: {message}", self.state.run_id))
    }
}

fn new_run_id() -> Result<String, AbFailure> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AbFailure::error("system clock is before Unix epoch"))?
        .as_nanos();
    Ok(format!(
        "ab-{nonce}-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn unix_ms() -> Result<u128, AbFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|_| AbFailure::error("system clock is before Unix epoch"))
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn create_lock(path: &Path) -> Result<File, AbFailure> {
    let lock_path = path.join(".lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|error| AbFailure::error(format!("could not create A/B lock: {error}")))?;
    lock.lock_exclusive()
        .map_err(|error| AbFailure::error(format!("could not lock A/B run: {error}")))?;
    Ok(lock)
}

fn open_lock(path: &Path) -> Result<File, AbFailure> {
    let lock_path = path.join(".lock");
    ensure_private_file(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| AbFailure::error(format!("could not open A/B lock: {error}")))?;
    lock.lock_exclusive()
        .map_err(|error| AbFailure::error(format!("could not lock A/B run: {error}")))?;
    Ok(lock)
}

fn validate_run_id(run_id: &str) -> Result<(), AbFailure> {
    if run_id.is_empty()
        || run_id.len() > 160
        || !run_id.starts_with("ab-")
        || !run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(AbFailure::error("invalid A/B run ID"));
    }
    Ok(())
}

fn ensure_private_existing_directory(path: &Path) -> Result<(), AbFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AbFailure::error(format!("could not inspect A/B state: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AbFailure::error("A/B state directory is missing or unsafe"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AbFailure::error(
                "A/B state directory is accessible to another user",
            ));
        }
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<(), AbFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AbFailure::error(format!("could not inspect A/B state file: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AbFailure::error("A/B state file is missing or unsafe"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AbFailure::error(
                "A/B state file is accessible to another user",
            ));
        }
    }
    Ok(())
}

fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<(), AbFailure> {
    let parent = path
        .parent()
        .ok_or_else(|| AbFailure::error("invalid A/B state path"))?;
    let temporary = parent.join(format!(
        ".experiment.tmp-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| AbFailure::error(format!("could not stage A/B state: {error}")))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(AbFailure::error(format!(
            "could not write A/B state: {error}"
        )));
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        AbFailure::error(format!("could not publish A/B state: {error}"))
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AbFailure::error(format!("could not sync A/B state: {error}")))
}
