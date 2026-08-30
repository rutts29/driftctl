use std::fs;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent::{display_path, run_codex};
use crate::codex_child::{ChildForkRequest, ChildTurnRequest, CodexChildAdapter};
use crate::codex_source::{self, SessionSelection};
use crate::goal_change_store::{GoalChangeStore, PendingGoalChange};
use crate::inspect_state::InspectSource;
use crate::intent_history::{Approval, Event, GoalRevision};
use crate::projection::{ActiveProjection, ProjectionConfig};
use crate::run_store::{RunStore, SourceCursorComparison};
use crate::semantic_resolver::{
    self, CompactorConfig, InspectResolution, NativeGoalObservation, ResolverFailureKind,
    ResolverMetadata, ResolverUsage, sanitized_human, sanitized_json,
};
use crate::session_bundle::NativeGoal;
use crate::{ClosureError, Ledger, Snapshot};

const USAGE: &str = "driftctl — durable continuity for coding-agent tasks\n\n\
Usage:\n\
  driftctl start --goal <text> --requirement <text> [--requirement <text> ...]\n\
  driftctl steer --requirement <text>\n\
  driftctl satisfy --id <requirement-id> --evidence <text>\n\
  driftctl status [--json]\n\
  driftctl resume [--json]\n\
  driftctl inspect codex (--last | --session <id>) [--json] [--compactor luna|terra|sol] [--reasoning high|medium]\n\
  driftctl bundle --run <run-id> --json\n\
  driftctl compare codex (--last | --session <id>) [--json]\n\
  driftctl continue codex (--last | --session <id>) [--approve-goal | --edit-goal <text> | --retain-goal | --cancel] [--json]\n\
  driftctl run codex\n\
  driftctl close";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            exit_code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            exit_code: 1,
            stdout: String::new(),
            stderr: message.into(),
        }
    }

    fn blocked(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            stdout: String::new(),
            stderr: message.into(),
        }
    }
}

#[must_use]
pub fn execute(root: &Path, arguments: impl IntoIterator<Item = String>) -> CliOutput {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return CliOutput::error(USAGE);
    };
    let arguments: Vec<String> = arguments.collect();

    let result = match command.as_str() {
        "--help" | "-h" | "help" => {
            if arguments.is_empty() {
                return CliOutput::success(USAGE);
            }
            Err("help does not accept arguments".to_owned())
        }
        "start" => start(root, &arguments),
        "steer" => steer(root, &arguments),
        "satisfy" => satisfy(root, &arguments),
        "status" | "resume" => status(root, &arguments),
        "inspect" => return inspect(root, &arguments),
        "bundle" => return bundle(root, &arguments),
        "compare" => return compare_codex(root, &arguments),
        "continue" => return continue_codex(root, &arguments),
        "run" => run(root, &arguments),
        "close" => return close(root, &arguments),
        _ => Err(format!("unknown command: {command}\n\n{USAGE}")),
    };

    match result {
        Ok(output) => CliOutput::success(output),
        Err(message) => CliOutput::error(message),
    }
}

fn inspect(root: &Path, arguments: &[String]) -> CliOutput {
    let parsed = parse_inspect_arguments(arguments);
    let Ok(options) = parsed else {
        return CliOutput::error(parsed.expect_err("checked error"));
    };
    let imported = match codex_source::inspect(root, options.selection) {
        Ok(imported) => imported,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let bundle = match imported.neutral_bundle() {
        Ok(bundle) => bundle,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let source = match InspectSource::from_bundle(&bundle) {
        Ok(source) => source,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let projection_config = match inspect_projection_config() {
        Ok(config) => config,
        Err(error) => return CliOutput::error(error),
    };
    let existing = match source.open(root) {
        Ok(existing) => existing,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    if let Some(mut existing) = existing {
        let comparison = match existing.recovered.source_cursor.as_ref() {
            Some(accepted) => accepted.compare(source.cursor()),
            None => {
                if !source.can_bind_unbound_initial_history(&existing.recovered.history) {
                    return CliOutput::error(
                        "stored inspect run is missing a source cursor and cannot be repaired safely",
                    );
                }
                if let Err(error) = existing.store.commit_projection_with_source_cursor(
                    &existing.recovered.projection,
                    source.cursor(),
                ) {
                    return CliOutput::error(error.to_string());
                }
                existing.recovered.source_cursor = Some(source.cursor().clone());
                Ok(SourceCursorComparison::Current)
            }
        };
        match comparison {
            Ok(SourceCursorComparison::Current) => {
                let goal_change = match load_goal_change(
                    &existing.store,
                    &existing.recovered,
                    source.cursor().digest(),
                ) {
                    Ok(goal_change) => goal_change,
                    Err(error) => return CliOutput::error(error),
                };
                let resolution =
                    cached_resolution(existing.recovered, Some(bundle.native_goal()), goal_change);
                return inspect_output(
                    &resolution,
                    source.run_id().as_str(),
                    source.cursor().accepted_record_count(),
                    source.cursor().digest(),
                    options.json,
                );
            }
            Ok(SourceCursorComparison::NewRecords(records)) => {
                let proposal_store = match GoalChangeStore::open(&existing.store) {
                    Ok(store) => store,
                    Err(error) => return CliOutput::error(error.to_string()),
                };
                let pending_goal_change = match proposal_store.load_pending() {
                    Ok(proposal) => proposal,
                    Err(error) => return CliOutput::error(error.to_string()),
                };
                if let Some(proposal) = pending_goal_change {
                    let event_sequence = match accepted_event_sequence(&existing.recovered) {
                        Ok(sequence) => sequence,
                        Err(error) => return CliOutput::error(error),
                    };
                    if !proposal.matches(
                        existing.recovered.projection.revision,
                        event_sequence,
                        source.cursor().digest(),
                    ) {
                        return CliOutput::error(
                            "pending goal-change proposal is stale for the observed source",
                        );
                    }
                    if let Err(error) = codex_source::verify_unchanged(root, &imported) {
                        return CliOutput::error(error.to_string());
                    }
                    if let Err(error) = existing.store.commit_projection_with_source_cursor(
                        &existing.recovered.projection,
                        source.cursor(),
                    ) {
                        return CliOutput::error(error.to_string());
                    }
                    let resolution = cached_resolution(
                        existing.recovered,
                        Some(bundle.native_goal()),
                        Some(proposal.observation()),
                    );
                    return inspect_output(
                        &resolution,
                        source.run_id().as_str(),
                        source.cursor().accepted_record_count(),
                        source.cursor().digest(),
                        options.json,
                    );
                }
                let accepted_count = source.cursor().accepted_record_count() - records.len();
                let delta = match source.delta_bundle(&bundle, accepted_count) {
                    Ok(delta) => delta,
                    Err(error) => return CliOutput::error(error.to_string()),
                };
                if let Err(error) = write_disclosure(options.compactor, 1) {
                    return CliOutput::error(error);
                }
                let resolution = match semantic_resolver::resolve_incremental(
                    root,
                    &existing.recovered.history,
                    &existing.recovered.projection,
                    &delta,
                    options.compactor,
                    projection_config,
                ) {
                    Ok(resolution) => resolution,
                    Err(failure) => return resolver_failure_output(failure, options.json),
                };
                if let Err(error) = codex_source::verify_unchanged(root, &imported) {
                    return CliOutput::error(error.to_string());
                }
                let accepted_events = existing.recovered.history.records().len();
                for record in &resolution.history.records()[accepted_events..] {
                    if let Err(error) = existing.store.append_pending(record.clone()) {
                        return CliOutput::error(error.to_string());
                    }
                }
                if let Some(goal_change) = &resolution.goal_change {
                    let proposal =
                        match PendingGoalChange::new(goal_change, source.cursor().digest()) {
                            Ok(proposal) => proposal,
                            Err(error) => return CliOutput::error(error.to_string()),
                        };
                    if let Err(error) = proposal_store.persist(&proposal) {
                        return CliOutput::error(error.to_string());
                    }
                }
                if let Err(error) = existing
                    .store
                    .commit_projection_with_source_cursor(&resolution.projection, source.cursor())
                {
                    return CliOutput::error(error.to_string());
                }
                return inspect_output(
                    &resolution,
                    source.run_id().as_str(),
                    source.cursor().accepted_record_count(),
                    source.cursor().digest(),
                    options.json,
                );
            }
            Ok(SourceCursorComparison::Stale { .. }) => {
                return CliOutput::error("source session is older than the accepted inspect run");
            }
            Ok(SourceCursorComparison::Rewrite { .. }) => {
                return CliOutput::error("accepted source history was rewritten");
            }
            Ok(SourceCursorComparison::SessionMismatch) => {
                return CliOutput::error("stored inspect run belongs to a different source");
            }
            Err(error) => return CliOutput::error(error.to_string()),
        }
    }
    let initial_chunks = match semantic_resolver::initial_chunk_count(&bundle) {
        Ok(chunks) => chunks,
        Err(error) => return CliOutput::error(error),
    };
    if let Err(error) = write_disclosure(options.compactor, initial_chunks) {
        return CliOutput::error(error);
    }
    match semantic_resolver::resolve_initial(root, &bundle, options.compactor, projection_config) {
        Ok(resolution) => {
            if let Err(error) = codex_source::verify_unchanged(root, &imported) {
                return CliOutput::error(error.to_string());
            }
            if let Err(error) = source.create(root, &resolution.history, &resolution.projection) {
                return CliOutput::error(error.to_string());
            }
            inspect_output(
                &resolution,
                source.run_id().as_str(),
                imported.imported_user_record_count(),
                &imported.source_digest(),
                options.json,
            )
        }
        Err(failure) => resolver_failure_output(failure, options.json),
    }
}

fn write_disclosure(compactor: CompactorConfig, chunks: usize) -> Result<(), String> {
    // This write intentionally occurs before spawning the paid provider call.
    // JSON mode reserves stdout for its single machine document.
    writeln!(
        std::io::stderr().lock(),
        "{}",
        compactor.disclosure_for_chunks(chunks)
    )
    .map_err(|_| "could not write compactor disclosure before provider call".to_owned())
}

fn resolver_failure_output(
    failure: semantic_resolver::ResolverFailure,
    json_output: bool,
) -> CliOutput {
    let error = match failure.kind {
        ResolverFailureKind::Execution => "compactor_execution_failed",
        ResolverFailureKind::InvalidProposal => "invalid_compactor_proposal",
    };
    let output = if json_output {
        serde_json::to_string(&json!({
            "schema_version":1,
            "status":"error",
            "error":error,
            "resolver":failure.metadata,
        }))
        .unwrap_or_else(|_| "{\"schema_version\":1,\"status\":\"error\"}".to_owned())
    } else {
        String::new()
    };
    CliOutput {
        exit_code: 1,
        stdout: output,
        stderr: error.to_owned(),
    }
}

fn cached_resolution(
    recovered: crate::run_store::RecoveredRun,
    native_goal: Option<&NativeGoal>,
    goal_change: Option<semantic_resolver::GoalChangeObservation>,
) -> InspectResolution {
    let generated = &recovered.projection.generated_by;
    let metadata = ResolverMetadata {
        model: generated
            .model
            .clone()
            .unwrap_or_else(|| "stored-projection".to_owned()),
        reasoning: generated
            .reasoning
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        calls: 0,
        elapsed_ms: 0,
        usage: ResolverUsage::default(),
        prompt_schema_version: generated.prompt_schema_version,
        proposal_schema_version: 1,
        last_validation_failure: None,
        artifact_ids: Vec::new(),
    };
    let text_private = native_goal.and_then(NativeGoal::text).map(str::to_owned);
    let native_goal = NativeGoalObservation {
        state: native_goal.map_or_else(|| "unknown".to_owned(), |goal| goal.state().to_owned()),
        conflicts_with_projection: text_private
            .as_deref()
            .is_some_and(|text| text != recovered.projection.goal.text),
        text_private,
    };
    InspectResolution {
        history: recovered.history,
        projection: recovered.projection,
        metadata,
        native_goal,
        goal_change,
    }
}

fn accepted_event_sequence(recovered: &crate::run_store::RecoveredRun) -> Result<u64, String> {
    recovered
        .history
        .records()
        .last()
        .map(|record| record.sequence)
        .ok_or_else(|| "stored inspect run has no accepted event sequence".to_owned())
}

fn load_goal_change(
    run: &RunStore,
    recovered: &crate::run_store::RecoveredRun,
    source_digest: &str,
) -> Result<Option<semantic_resolver::GoalChangeObservation>, String> {
    let store = GoalChangeStore::open(run).map_err(|error| error.to_string())?;
    let Some(proposal) = store.load_pending().map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let event_sequence = accepted_event_sequence(recovered)?;
    if !proposal.matches(recovered.projection.revision, event_sequence, source_digest) {
        return Err("pending goal-change proposal is stale for the accepted source".to_owned());
    }
    Ok(Some(proposal.observation()))
}

fn inspect_output(
    resolution: &InspectResolution,
    run_id: &str,
    imported_user_records: usize,
    source_digest: &str,
    json_output: bool,
) -> CliOutput {
    let output = if json_output {
        sanitized_json(resolution, imported_user_records, source_digest)
            .and_then(|document| insert_run_id(&document, run_id))
            .unwrap_or_else(|_| {
                "{\"schema_version\":1,\"status\":\"error\",\"error\":\"serialization_failed\"}"
                    .to_owned()
            })
    } else {
        format!(
            "run id: {run_id}\n{}",
            sanitized_human(resolution, imported_user_records, source_digest)
        )
    };
    CliOutput {
        exit_code: if resolution.continuation_blocked() {
            2
        } else {
            0
        },
        stdout: output,
        stderr: String::new(),
    }
}

fn insert_run_id(document: &str, run_id: &str) -> Result<String, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_str(document)?;
    let Some(object) = value.as_object_mut() else {
        return serde_json::to_string(&value);
    };
    object.insert("run_id".to_owned(), json!(run_id));
    serde_json::to_string(&value)
}

fn bundle(root: &Path, arguments: &[String]) -> CliOutput {
    let [run_flag, run_id, json_flag] = arguments else {
        return CliOutput::error("bundle requires exactly: --run <run-id> --json");
    };
    if run_flag != "--run" || json_flag != "--json" {
        return CliOutput::error("bundle requires exactly: --run <run-id> --json");
    }
    let store = match RunStore::open_default(root, run_id) {
        Ok(store) => store,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let recovered = match store.recover() {
        Ok(recovered) => recovered,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let Some(cursor) = recovered.source_cursor.as_ref() else {
        return CliOutput::error("stored inspect run has no accepted source cursor");
    };
    let goal_change = match load_goal_change(&store, &recovered, cursor.digest()) {
        Ok(goal_change) => goal_change,
        Err(error) => return CliOutput::error(error),
    };
    let resolution = cached_resolution(recovered.clone(), None, goal_change);
    let document =
        match sanitized_json(&resolution, cursor.accepted_record_count(), cursor.digest())
            .and_then(|document| serde_json::from_str::<serde_json::Value>(&document))
        {
            Ok(document) => document,
            Err(_) => return CliOutput::error("could not serialize sanitized run bundle"),
        };
    let output = json!({
        "schema_version": 1,
        "run_id": run_id,
        "status": document["status"],
        "projection": document["projection"],
        "blockers": document["blockers"],
    });
    match serde_json::to_string(&output) {
        Ok(output) => CliOutput::success(output),
        Err(_) => CliOutput::error("could not serialize sanitized run bundle"),
    }
}

fn compare_codex(root: &Path, arguments: &[String]) -> CliOutput {
    let options = match parse_continue_arguments(arguments) {
        Ok(options) if options.action.is_none() => options,
        Ok(_) => return CliOutput::error("compare does not accept goal-decision options"),
        Err(error) => return CliOutput::error(error.replace("continue", "compare")),
    };
    let inspect_arguments = options.inspect_arguments();
    let inspection = inspect(root, &inspect_arguments);
    if inspection.exit_code != 0 {
        return inspection;
    }
    let document: Value = match serde_json::from_str(&inspection.stdout) {
        Ok(document) => document,
        Err(_) => return CliOutput::error("could not read inspect result before comparison"),
    };
    let Some(run_id) = document["run_id"].as_str() else {
        return CliOutput::error("inspect result has no run ID");
    };
    let store = match RunStore::open_default(root, run_id) {
        Ok(store) => store,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let recovered = match store.recover() {
        Ok(recovered) => recovered,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let Some(cursor) = recovered.source_cursor.as_ref() else {
        return CliOutput::error("stored inspect run has no accepted source cursor");
    };
    let imported = match codex_source::inspect(root, options.selection.as_borrowed()) {
        Ok(imported) => imported,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let source_bundle = match imported.neutral_bundle() {
        Ok(bundle) => bundle,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let observed_source = match InspectSource::from_bundle(&source_bundle) {
        Ok(source) => source,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    if observed_source.run_id().as_str() != run_id
        || observed_source.cursor().digest() != cursor.digest()
    {
        return CliOutput::blocked("source changed after inspect; inspect again before comparing");
    }

    let candidate_parent = store.path().join("comparisons");
    if let Err(error) = create_private_directory(&candidate_parent) {
        return CliOutput::error(error);
    }
    let pair = match crate::workspace::isolate_workspace(root, &candidate_parent) {
        Ok(pair) => pair,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let adapter = CodexChildAdapter::from_environment();
    let goal = recovered.projection.goal.text.clone();
    let baseline_request = match ChildForkRequest::new(
        source_bundle.source().session_ref_private(),
        pair.baseline().root(),
        &goal,
    ) {
        Ok(request) => request,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let workflow_request = match ChildForkRequest::new(
        source_bundle.source().session_ref_private(),
        pair.workflow().root(),
        &goal,
    ) {
        Ok(request) => request,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let baseline = match adapter.fork_and_migrate(baseline_request) {
        Ok(migration) => migration,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    let workflow = match adapter.fork_and_migrate(workflow_request) {
        Ok(migration) => migration,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    let neutral_prompt = "Continue the task from this checkpoint. Preserve existing behavior and complete the remaining work. Do not claim completion without running relevant validation.";
    let baseline_turn = match ChildTurnRequest::without_projection(
        baseline.child_id(),
        baseline.child_cwd(),
        neutral_prompt,
    )
    .and_then(|request| adapter.start_child_turn(request))
    {
        Ok(turn) => turn,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    let projection_context = match public_projection_context(&recovered) {
        Ok(context) => context,
        Err(error) => return CliOutput::error(error),
    };
    let workflow_turn = match ChildTurnRequest::new(
        workflow.child_id(),
        workflow.child_cwd(),
        neutral_prompt,
        projection_context,
    )
    .and_then(|request| adapter.start_child_turn(request))
    {
        Ok(turn) => turn,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    if let Err(error) = codex_source::verify_unchanged(root, &imported) {
        return CliOutput::blocked(error.to_string());
    }
    let baseline_diff = match crate::workspace::candidate_diff(baseline.child_cwd()) {
        Ok(diff) => diff,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let workflow_diff = match crate::workspace::candidate_diff(workflow.child_cwd()) {
        Ok(diff) => diff,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let pair_completed = baseline_turn.completed() && workflow_turn.completed();
    let output = json!({
        "schema_version":1,
        "status":if pair_completed { "completed" } else { "invalid_pair" },
        "run_id":run_id,
        "fairness":{
            "starting_manifest_equal":pair.baseline().manifest() == pair.workflow().manifest(),
            "neutral_prompt_equal":true,
            "worker_policy":"inherited_from_parent",
            "only_intended_input_difference":"workflow receives the bounded active-intent projection",
        },
        "baseline":{
            "child_thread_id":baseline.child_id(),
            "child_cwd":baseline.child_cwd(),
            "turn_id":baseline_turn.turn_id(),
            "turn_status":format!("{:?}", baseline_turn.status()).to_ascii_lowercase(),
            "changed_paths":baseline_diff.changed_paths(),
        },
        "workflow":{
            "child_thread_id":workflow.child_id(),
            "child_cwd":workflow.child_cwd(),
            "turn_id":workflow_turn.turn_id(),
            "turn_status":format!("{:?}", workflow_turn.status()).to_ascii_lowercase(),
            "changed_paths":workflow_diff.changed_paths(),
        },
        "source_unchanged":true,
        "parent_unchanged":true,
        "adoption":"none",
    });
    let exit_code = if pair_completed { 0 } else { 2 };
    if options.json {
        match serde_json::to_string(&output) {
            Ok(stdout) => CliOutput {
                exit_code,
                stdout,
                stderr: String::new(),
            },
            Err(_) => CliOutput::error("could not serialize comparison result"),
        }
    } else {
        CliOutput {
            exit_code,
            stdout: format!(
                "comparison {}: baseline {}, workflow {}; adoption: none",
                if pair_completed {
                    "completed"
                } else {
                    "invalid"
                },
                baseline_turn.turn_id(),
                workflow_turn.turn_id(),
            ),
            stderr: String::new(),
        }
    }
}

fn continue_codex(root: &Path, arguments: &[String]) -> CliOutput {
    let options = match parse_continue_arguments(arguments) {
        Ok(options) => options,
        Err(error) => return CliOutput::error(error),
    };
    let inspect_arguments = options.inspect_arguments();
    let inspection = inspect(root, &inspect_arguments);
    if inspection.exit_code == 1 {
        return inspection;
    }
    let document: Value = match serde_json::from_str(&inspection.stdout) {
        Ok(document) => document,
        Err(_) => return CliOutput::error("could not read inspect result before continuation"),
    };
    let blockers = document["blockers"].as_array().cloned().unwrap_or_default();
    if blockers.iter().any(|blocker| {
        !matches!(
            blocker["kind"].as_str(),
            Some("goal_change_pending" | "native_goal_conflict")
        )
    }) {
        return inspection;
    }
    let Some(run_id) = document["run_id"].as_str() else {
        return CliOutput::error("inspect result has no run ID");
    };
    let store = match RunStore::open_default(root, run_id) {
        Ok(store) => store,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let recovered = match store.recover() {
        Ok(recovered) => recovered,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let Some(cursor) = recovered.source_cursor.as_ref() else {
        return CliOutput::error("stored inspect run has no accepted source cursor");
    };
    let proposal_store = match GoalChangeStore::open(&store) {
        Ok(store) => store,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let pending = match proposal_store.load_pending() {
        Ok(pending) => pending,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    if let Some(proposal) = pending.as_ref() {
        let sequence = match accepted_event_sequence(&recovered) {
            Ok(sequence) => sequence,
            Err(error) => return CliOutput::error(error),
        };
        if !proposal.matches(recovered.projection.revision, sequence, cursor.digest()) {
            return CliOutput::blocked("pending goal-change decision is stale");
        }
    }

    let action = match options.action.clone() {
        Some(action) => action,
        None if pending.is_none() && inspection.exit_code == 0 => ContinueAction::UseCurrent,
        None if !std::io::stdin().is_terminal() => {
            return CliOutput {
                exit_code: 2,
                stdout: inspection.stdout,
                stderr: "operator decision required; rerun with --approve-goal, --edit-goal <text>, --retain-goal, or --cancel".to_owned(),
            };
        }
        None => match prompt_for_goal_action(pending.as_ref()) {
            Ok(action) => action,
            Err(error) => return CliOutput::blocked(error),
        },
    };

    let (approved_goal, approving_pending) = match action {
        ContinueAction::Approve => match pending.as_ref() {
            Some(proposal) => (proposal.proposed_goal().to_owned(), true),
            None => (recovered.projection.goal.text.clone(), false),
        },
        ContinueAction::Edit(goal) => {
            let Some(proposal) = pending.as_ref() else {
                return CliOutput::error("there is no pending goal change to edit");
            };
            if let Err(error) = proposal_store.edit(proposal, &goal) {
                return CliOutput::error(error.to_string());
            }
            drop(proposal_store);
            drop(store);
            return inspect(root, &inspect_arguments);
        }
        ContinueAction::Retain => {
            let Some(proposal) = pending.as_ref() else {
                return CliOutput::error("there is no pending goal change to reject");
            };
            if let Err(error) = proposal_store.reject(proposal) {
                return CliOutput::error(error.to_string());
            }
            (recovered.projection.goal.text.clone(), false)
        }
        ContinueAction::Cancel => return inspection,
        ContinueAction::UseCurrent => (recovered.projection.goal.text.clone(), false),
    };

    let imported = match codex_source::inspect(root, options.selection.as_borrowed()) {
        Ok(imported) => imported,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let source_bundle = match imported.neutral_bundle() {
        Ok(bundle) => bundle,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let observed_source = match InspectSource::from_bundle(&source_bundle) {
        Ok(source) => source,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    if observed_source.run_id().as_str() != run_id
        || observed_source.cursor().digest() != cursor.digest()
    {
        return CliOutput::blocked("source changed after inspect; inspect again before continuing");
    }

    let candidate_parent = store.path().join("workspaces");
    if let Err(error) = create_private_directory(&candidate_parent) {
        return CliOutput::error(error);
    }
    let pair = match crate::workspace::isolate_workspace(root, &candidate_parent) {
        Ok(pair) => pair,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let adapter = CodexChildAdapter::from_environment();
    let request = match ChildForkRequest::new(
        source_bundle.source().session_ref_private(),
        pair.workflow().root(),
        &approved_goal,
    ) {
        Ok(request) => request,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let migration = match adapter.fork_and_migrate(request) {
        Ok(migration) => migration,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    if let Err(error) = codex_source::verify_unchanged(root, &imported) {
        return CliOutput::blocked(error.to_string());
    }

    let final_recovered = if approving_pending {
        let proposal = pending.as_ref().expect("approval has pending proposal");
        match apply_approved_goal(&store, &recovered, proposal) {
            Ok(recovered) => {
                if let Err(error) = proposal_store.mark_applied(proposal) {
                    return CliOutput::blocked(error.to_string());
                }
                recovered
            }
            Err(error) => return CliOutput::blocked(error),
        }
    } else {
        recovered
    };
    let projection_context = match public_projection_context(&final_recovered) {
        Ok(context) => context,
        Err(error) => return CliOutput::error(error),
    };
    let turn_request = match ChildTurnRequest::new(
        migration.child_id(),
        migration.child_cwd(),
        "Continue the accepted task from this bounded active-intent projection. Preserve every listed invariant and do not claim completion without the listed validation evidence.",
        projection_context,
    ) {
        Ok(request) => request,
        Err(error) => return CliOutput::error(error.to_string()),
    };
    let turn = match adapter.start_child_turn(turn_request) {
        Ok(turn) => turn,
        Err(error) => return CliOutput::blocked(error.to_string()),
    };
    if let Err(error) = codex_source::verify_unchanged(root, &imported) {
        return CliOutput::blocked(error.to_string());
    }

    let output = json!({
        "schema_version":1,
        "status":"started",
        "run_id":run_id,
        "child_thread_id":migration.child_id(),
        "child_cwd":migration.child_cwd(),
        "goal":approved_goal,
        "turn_id":turn.turn_id(),
        "turn_status":format!("{:?}", turn.status()).to_ascii_lowercase(),
        "parent_unchanged":true,
        "source_unchanged":true,
        "adoption":"manual",
    });
    if options.json {
        match serde_json::to_string(&output) {
            Ok(output) => CliOutput::success(output),
            Err(_) => CliOutput::error("could not serialize continuation result"),
        }
    } else {
        CliOutput::success(format!(
            "started child {} in {}\nparent and source unchanged; adoption remains manual",
            migration.child_id(),
            migration.child_cwd().display()
        ))
    }
}

fn apply_approved_goal(
    store: &RunStore,
    recovered: &crate::run_store::RecoveredRun,
    proposal: &PendingGoalChange,
) -> Result<crate::run_store::RecoveredRun, String> {
    let mut history = recovered.history.clone();
    let current = history.goal();
    let mut goal = GoalRevision::new(
        current.revision.saturating_add(1),
        proposal.proposed_goal(),
        proposal.source_refs().to_vec(),
    );
    goal.supersedes_revision = Some(current.revision);
    goal.approval = Some(Approval::from_sources(proposal.source_refs().to_vec()));
    history
        .append(Event::GoalRevised { goal })
        .map_err(|error| error.to_string())?;
    let record = history
        .records()
        .last()
        .cloned()
        .ok_or_else(|| "approved goal revision produced no history event".to_owned())?;
    store
        .append_pending(record)
        .map_err(|error| error.to_string())?;
    let mut projection = ActiveProjection::from_history(
        &history,
        ProjectionConfig::new(recovered.projection.overflow.budget),
    )
    .map_err(|error| error.to_string())?;
    projection.generated_by = recovered.projection.generated_by.clone();
    let cursor = recovered
        .source_cursor
        .as_ref()
        .ok_or_else(|| "stored inspect run has no accepted source cursor".to_owned())?;
    store
        .commit_projection_with_source_cursor(&projection, cursor)
        .map_err(|error| error.to_string())?;
    store.recover().map_err(|error| error.to_string())
}

fn public_projection_context(recovered: &crate::run_store::RecoveredRun) -> Result<String, String> {
    let cursor = recovered
        .source_cursor
        .as_ref()
        .ok_or_else(|| "stored inspect run has no accepted source cursor".to_owned())?;
    let resolution = cached_resolution(recovered.clone(), None, None);
    let document = sanitized_json(&resolution, cursor.accepted_record_count(), cursor.digest())
        .map_err(|error| error.to_string())?;
    let document: Value = serde_json::from_str(&document).map_err(|error| error.to_string())?;
    serde_json::to_string(&document["projection"]).map_err(|error| error.to_string())
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err("workspace state path is not a private directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|error| format!("could not create workspace state: {error}"))?;
        }
        Err(error) => return Err(format!("could not inspect workspace state: {error}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not protect workspace state: {error}"))?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContinueAction {
    Approve,
    Edit(String),
    Retain,
    Cancel,
    UseCurrent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OwnedSessionSelection {
    Last,
    Session(String),
}

impl OwnedSessionSelection {
    fn as_borrowed(&self) -> SessionSelection<'_> {
        match self {
            Self::Last => SessionSelection::Last,
            Self::Session(id) => SessionSelection::Explicit(id),
        }
    }
}

struct ContinueOptions {
    selection: OwnedSessionSelection,
    action: Option<ContinueAction>,
    json: bool,
}

impl ContinueOptions {
    fn inspect_arguments(&self) -> Vec<String> {
        let mut arguments = vec!["codex".to_owned()];
        match &self.selection {
            OwnedSessionSelection::Last => arguments.push("--last".to_owned()),
            OwnedSessionSelection::Session(id) => {
                arguments.push("--session".to_owned());
                arguments.push(id.clone());
            }
        }
        arguments.push("--json".to_owned());
        arguments
    }
}

fn parse_continue_arguments(arguments: &[String]) -> Result<ContinueOptions, String> {
    if arguments.first().map(String::as_str) != Some("codex") {
        return Err("continue requires provider codex".to_owned());
    }
    let mut selection = None;
    let mut action = None;
    let mut json = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--last" if selection.is_none() => selection = Some(OwnedSessionSelection::Last),
            "--session" if selection.is_none() => {
                index += 1;
                let id = arguments
                    .get(index)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "--session requires an ID".to_owned())?;
                selection = Some(OwnedSessionSelection::Session(id.clone()));
            }
            "--approve-goal" if action.is_none() => action = Some(ContinueAction::Approve),
            "--edit-goal" if action.is_none() => {
                index += 1;
                let goal = arguments
                    .get(index)
                    .filter(|goal| !goal.trim().is_empty())
                    .ok_or_else(|| "--edit-goal requires text".to_owned())?;
                action = Some(ContinueAction::Edit(goal.clone()));
            }
            "--retain-goal" if action.is_none() => action = Some(ContinueAction::Retain),
            "--cancel" if action.is_none() => action = Some(ContinueAction::Cancel),
            "--json" if !json => json = true,
            option => return Err(format!("unsupported or repeated continue option: {option}")),
        }
        index += 1;
    }
    Ok(ContinueOptions {
        selection: selection
            .ok_or_else(|| "continue requires --last or --session <id>".to_owned())?,
        action,
        json,
    })
}

fn prompt_for_goal_action(pending: Option<&PendingGoalChange>) -> Result<ContinueAction, String> {
    if let Some(proposal) = pending {
        eprintln!("proposed goal: {}", proposal.proposed_goal());
        eprintln!("approve, edit, retain, or cancel? [a/e/r/c]");
    } else {
        eprintln!("approve migration of the accepted goal to an isolated child? [a/c]");
    }
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("could not read operator decision: {error}"))?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "a" | "approve" => Ok(ContinueAction::Approve),
        "r" | "retain" if pending.is_some() => Ok(ContinueAction::Retain),
        "c" | "cancel" => Ok(ContinueAction::Cancel),
        "e" | "edit" if pending.is_some() => {
            eprintln!("new goal:");
            let mut goal = String::new();
            std::io::stdin()
                .read_line(&mut goal)
                .map_err(|error| format!("could not read edited goal: {error}"))?;
            if goal.trim().is_empty() {
                Err("edited goal must not be empty".to_owned())
            } else {
                Ok(ContinueAction::Edit(goal.trim().to_owned()))
            }
        }
        _ => Err("operator cancelled: unrecognized decision".to_owned()),
    }
}

#[derive(Debug)]
struct InspectOptions<'a> {
    selection: SessionSelection<'a>,
    json: bool,
    compactor: CompactorConfig,
}

fn parse_inspect_arguments(arguments: &[String]) -> Result<InspectOptions<'_>, String> {
    let Some(provider) = arguments.first() else {
        return Err("inspect requires provider `codex`".to_owned());
    };
    if provider != "codex" {
        return Err("inspect currently supports exactly: driftctl inspect codex".to_owned());
    }
    let mut selection = None;
    let mut json = false;
    let mut compactor = None;
    let mut reasoning = None;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--last" if selection.is_none() => {
                selection = Some(SessionSelection::Last);
                index += 1;
            }
            "--session" if selection.is_none() => {
                let Some(id) = arguments.get(index + 1) else {
                    return Err("missing value for --session".to_owned());
                };
                selection = Some(SessionSelection::Explicit(id));
                index += 2;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--compactor" if compactor.is_none() => {
                let Some(value) = arguments.get(index + 1) else {
                    return Err("missing value for --compactor".to_owned());
                };
                compactor = Some(value.as_str());
                index += 2;
            }
            "--reasoning" if reasoning.is_none() => {
                let Some(value) = arguments.get(index + 1) else {
                    return Err("missing value for --reasoning".to_owned());
                };
                reasoning = Some(value.as_str());
                index += 2;
            }
            "--last" | "--session" => {
                return Err("inspect requires exactly one of --last or --session <id>".to_owned());
            }
            "--json" => return Err("--json may only be supplied once".to_owned()),
            "--compactor" => return Err("--compactor may only be supplied once".to_owned()),
            "--reasoning" => return Err("--reasoning may only be supplied once".to_owned()),
            option => return Err(format!("unknown inspect option: {option}")),
        }
    }
    let selection = selection
        .ok_or_else(|| "inspect requires exactly one of --last or --session <id>".to_owned())?;
    let compactor = CompactorConfig::new(compactor.unwrap_or("luna"), reasoning)?;
    Ok(InspectOptions {
        selection,
        json,
        compactor,
    })
}

fn inspect_projection_config() -> Result<ProjectionConfig, String> {
    let Some(value) = std::env::var_os("DRIFTCTL_PROJECTION_BYTE_BUDGET") else {
        return Ok(ProjectionConfig::default());
    };
    let value = value
        .to_str()
        .ok_or_else(|| "DRIFTCTL_PROJECTION_BYTE_BUDGET must be valid UTF-8".to_owned())?;
    let budget = value
        .parse::<usize>()
        .map_err(|_| "DRIFTCTL_PROJECTION_BYTE_BUDGET must be a positive integer".to_owned())?;
    if budget == 0 {
        return Err("DRIFTCTL_PROJECTION_BYTE_BUDGET must be a positive integer".to_owned());
    }
    Ok(ProjectionConfig::new(budget))
}

fn start(root: &Path, arguments: &[String]) -> Result<String, String> {
    let mut goal = None;
    let mut requirements = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--goal" if goal.is_none() => goal = Some(value.clone()),
            "--goal" => return Err("--goal may only be supplied once".to_owned()),
            "--requirement" => requirements.push(value.clone()),
            _ => return Err(format!("unknown start option: {flag}")),
        }
        index += 2;
    }

    let goal = goal.ok_or_else(|| "start requires --goal".to_owned())?;
    if requirements.is_empty() {
        return Err("start requires at least one --requirement".to_owned());
    }

    let state_directory = state_directory(root);
    fs::create_dir_all(&state_directory).map_err(|error| error.to_string())?;
    let requirements = requirements
        .into_iter()
        .enumerate()
        .map(|(index, text)| (format!("R{}", index + 1), text));
    Ledger::create(ledger_path(root), goal, requirements).map_err(|error| error.to_string())?;
    Ok("started".to_owned())
}

fn steer(root: &Path, arguments: &[String]) -> Result<String, String> {
    let requirement = required_option(arguments, "--requirement")?;
    let mut ledger = open_ledger(root)?;
    ledger.steer(requirement).map_err(|error| error.to_string())
}

fn satisfy(root: &Path, arguments: &[String]) -> Result<String, String> {
    let options = required_options(arguments, &["--id", "--evidence"])?;
    let requirement_id = options[0].as_str();
    let evidence = options[1].as_str();
    let mut ledger = open_ledger(root)?;
    ledger
        .satisfy(requirement_id, evidence)
        .map_err(|error| error.to_string())?;
    Ok(format!("satisfied {requirement_id}"))
}

fn status(root: &Path, arguments: &[String]) -> Result<String, String> {
    let json = match arguments {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err("status and resume accept only --json".to_owned()),
    };
    let snapshot = open_ledger(root)?
        .snapshot()
        .map_err(|error| error.to_string())?;
    render_snapshot(&snapshot, json)
}

fn run(root: &Path, arguments: &[String]) -> Result<String, String> {
    match arguments {
        [agent] if agent == "codex" => {}
        _ => return Err("run currently supports exactly: driftctl run codex".to_owned()),
    }
    let snapshot = open_ledger(root)?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let trajectory = run_codex(root, &snapshot).map_err(|error| error.to_string())?;
    Ok(format!("trajectory: {}", display_path(root, &trajectory)))
}

fn close(root: &Path, arguments: &[String]) -> CliOutput {
    if !arguments.is_empty() {
        return CliOutput::error("close does not accept arguments");
    }
    let mut ledger = match open_ledger(root) {
        Ok(ledger) => ledger,
        Err(message) => return CliOutput::error(message),
    };
    match ledger.close() {
        Ok(()) => CliOutput::success("verified"),
        Err(ClosureError::UnresolvedRequirements(ids)) => CliOutput::blocked(format!(
            "closure blocked; unresolved requirements: {}",
            ids.join(", ")
        )),
        Err(ClosureError::Ledger(message)) => CliOutput::error(message),
    }
}

fn required_option(arguments: &[String], flag: &str) -> Result<String, String> {
    let options = required_options(arguments, &[flag])?;
    Ok(options[0].clone())
}

fn required_options(arguments: &[String], flags: &[&str]) -> Result<Vec<String>, String> {
    if arguments.len() != flags.len() * 2 {
        return Err(format!("required options: {}", flags.join(", ")));
    }

    let mut values = Vec::with_capacity(flags.len());
    for expected in flags {
        let position = arguments
            .iter()
            .position(|argument| argument == expected)
            .ok_or_else(|| format!("missing required option: {expected}"))?;
        let value = arguments
            .get(position + 1)
            .ok_or_else(|| format!("missing value for {expected}"))?;
        if flags.contains(&value.as_str()) {
            return Err(format!("missing value for {expected}"));
        }
        values.push(value.clone());
    }
    Ok(values)
}

fn open_ledger(root: &Path) -> Result<Ledger, String> {
    Ledger::open(ledger_path(root))
        .map_err(|error| format!("could not open drift state; run `driftctl start` first: {error}"))
}

fn render_snapshot(snapshot: &Snapshot, as_json: bool) -> Result<String, String> {
    let requirements: Vec<_> = snapshot
        .requirements()
        .iter()
        .map(|requirement| {
            json!({
                "id": requirement.id(),
                "text": requirement.text(),
                "satisfied": requirement.evidence().is_some(),
                "evidence": requirement.evidence(),
            })
        })
        .collect();
    if as_json {
        return serde_json::to_string(&json!({
            "goal": snapshot.goal(),
            "unresolved": snapshot.unresolved_requirement_ids(),
            "requirements": requirements,
            "closed": snapshot.is_closed(),
        }))
        .map_err(|error| error.to_string());
    }

    let unresolved = if snapshot.unresolved_requirement_ids().is_empty() {
        "none".to_owned()
    } else {
        snapshot.unresolved_requirement_ids().join(", ")
    };
    let mut output = format!(
        "goal: {}\nunresolved: {unresolved}\nrequirements:",
        snapshot.goal()
    );
    for requirement in snapshot.requirements() {
        let status = if requirement.evidence().is_some() {
            "satisfied"
        } else {
            "unresolved"
        };
        output.push_str(&format!(
            "\n- {} [{status}]: {}",
            requirement.id(),
            requirement.text()
        ));
        if let Some(evidence) = requirement.evidence() {
            output.push_str(&format!("\n  evidence: {evidence}"));
        }
    }
    output.push_str(&format!("\nclosed: {}", snapshot.is_closed()));
    Ok(output)
}

fn state_directory(root: &Path) -> PathBuf {
    root.join(".driftctl")
}

fn ledger_path(root: &Path) -> PathBuf {
    state_directory(root).join("ledger.jsonl")
}
