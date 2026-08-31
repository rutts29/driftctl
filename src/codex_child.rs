//! Bounded Codex App Server operations for isolated child continuations.
//!
//! This module deliberately owns no workspace lifecycle and never falls back
//! to interactive slash commands. It only proves the provider-side child and
//! goal transition that a higher-level continuation workflow has approved.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

const MAX_PROTOCOL_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoalObservation {
    Absent,
    Known(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManualGoalState {
    Absent,
    Known(String),
    Unknown,
}

impl From<GoalObservation> for ManualGoalState {
    fn from(observation: GoalObservation) -> Self {
        match observation {
            GoalObservation::Absent => Self::Absent,
            GoalObservation::Known(objective) => Self::Known(objective),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualGoalHandoff {
    child_id: String,
    child_cwd: PathBuf,
    observed_goal: ManualGoalState,
    intended_goal: String,
    failure: String,
}

impl ManualGoalHandoff {
    pub fn child_id(&self) -> &str {
        &self.child_id
    }

    pub fn child_cwd(&self) -> &Path {
        &self.child_cwd
    }

    pub fn observed_goal(&self) -> &ManualGoalState {
        &self.observed_goal
    }

    pub fn intended_goal(&self) -> &str {
        &self.intended_goal
    }

    pub fn failure(&self) -> &str {
        &self.failure
    }

    pub fn requires_new_approval(&self) -> bool {
        true
    }

    pub fn resume_argv(&self) -> [&str; 3] {
        ["codex", "resume", &self.child_id]
    }

    pub fn slash_commands(&self) -> [&str; 2] {
        ["/goal clear", "/goal"]
    }
}

impl GoalObservation {
    pub fn objective(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Known(objective) => Some(objective),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerPolicy {
    model: String,
    effort: String,
}

impl WorkerPolicy {
    pub fn new(
        model: impl Into<String>,
        effort: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        Ok(Self {
            model: validate_identifier(model.into(), "worker model")?,
            effort: validate_identifier(effort.into(), "worker reasoning effort")?,
        })
    }

    pub fn luna_max() -> Self {
        Self {
            model: "gpt-5.6-luna".to_owned(),
            effort: "max".to_owned(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn effort(&self) -> &str {
        &self.effort
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildForkRequest {
    parent_thread_id: String,
    isolated_cwd: PathBuf,
    approved_objective: String,
    worker_policy: WorkerPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreservedForkRequest {
    parent_thread_id: String,
    isolated_cwd: PathBuf,
    worker_policy: WorkerPolicy,
    last_turn_id: Option<String>,
}

impl PreservedForkRequest {
    pub fn new(
        parent_thread_id: impl Into<String>,
        isolated_cwd: impl AsRef<Path>,
    ) -> Result<Self, ChildAdapterError> {
        Ok(Self {
            parent_thread_id: validate_identifier(parent_thread_id.into(), "parent thread ID")?,
            isolated_cwd: canonical_child_cwd(isolated_cwd.as_ref())?,
            worker_policy: WorkerPolicy::luna_max(),
            last_turn_id: None,
        })
    }

    pub fn with_worker_policy(mut self, worker_policy: WorkerPolicy) -> Self {
        self.worker_policy = worker_policy;
        self
    }

    pub fn through_turn(
        mut self,
        last_turn_id: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        self.last_turn_id = Some(validate_identifier(
            last_turn_id.into(),
            "selected turn ID",
        )?);
        Ok(self)
    }
}

impl ChildForkRequest {
    pub fn new(
        parent_thread_id: impl Into<String>,
        isolated_cwd: impl AsRef<Path>,
        approved_objective: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        let parent_thread_id = validate_identifier(parent_thread_id.into(), "parent thread ID")?;
        let isolated_cwd = canonical_child_cwd(isolated_cwd.as_ref())?;
        let approved_objective = validate_objective(approved_objective.into())?;
        Ok(Self {
            parent_thread_id,
            isolated_cwd,
            approved_objective,
            worker_policy: WorkerPolicy::luna_max(),
        })
    }

    pub fn with_worker_policy(mut self, worker_policy: WorkerPolicy) -> Self {
        self.worker_policy = worker_policy;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildMigration {
    parent_goal: GoalObservation,
    child_id: String,
    child_cwd: PathBuf,
    child_goal: GoalObservation,
}

impl ChildMigration {
    pub fn parent_goal(&self) -> &GoalObservation {
        &self.parent_goal
    }

    pub fn child_id(&self) -> &str {
        &self.child_id
    }

    pub fn child_cwd(&self) -> &Path {
        &self.child_cwd
    }

    pub fn child_goal(&self) -> &GoalObservation {
        &self.child_goal
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildTurnRequest {
    child_thread_id: String,
    child_cwd: PathBuf,
    neutral_prompt: String,
    projection: Option<String>,
    worker_policy: WorkerPolicy,
}

impl ChildTurnRequest {
    pub fn new(
        child_thread_id: impl Into<String>,
        child_cwd: impl AsRef<Path>,
        neutral_prompt: impl Into<String>,
        projection: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        let child_thread_id = validate_identifier(child_thread_id.into(), "child thread ID")?;
        let child_cwd = child_cwd.as_ref().canonicalize().map_err(|error| {
            ChildAdapterError::protocol(format!("could not canonicalize child cwd: {error}"))
        })?;
        if !child_cwd.is_dir() {
            return Err(ChildAdapterError::protocol(
                "child cwd must be an existing directory",
            ));
        }
        let neutral_prompt = validate_text(neutral_prompt.into(), "neutral prompt")?;
        let projection = validate_text(projection.into(), "projection")?;
        Ok(Self {
            child_thread_id,
            child_cwd,
            neutral_prompt,
            projection: Some(projection),
            worker_policy: WorkerPolicy::luna_max(),
        })
    }

    pub fn without_projection(
        child_thread_id: impl Into<String>,
        child_cwd: impl AsRef<Path>,
        neutral_prompt: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        let child_thread_id = validate_identifier(child_thread_id.into(), "child thread ID")?;
        let child_cwd = child_cwd.as_ref().canonicalize().map_err(|error| {
            ChildAdapterError::protocol(format!("could not canonicalize child cwd: {error}"))
        })?;
        if !child_cwd.is_dir() {
            return Err(ChildAdapterError::protocol(
                "child cwd must be an existing directory",
            ));
        }
        Ok(Self {
            child_thread_id,
            child_cwd,
            neutral_prompt: validate_text(neutral_prompt.into(), "neutral prompt")?,
            projection: None,
            worker_policy: WorkerPolicy::luna_max(),
        })
    }

    pub fn with_worker_policy(mut self, worker_policy: WorkerPolicy) -> Self {
        self.worker_policy = worker_policy;
        self
    }

    fn text(&self) -> String {
        self.projection.as_ref().map_or_else(
            || self.neutral_prompt.clone(),
            |projection| format!("{}\n\n{}", self.neutral_prompt, projection),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildTurnStatus {
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildTurnStart {
    turn_id: String,
    status: ChildTurnStatus,
}

impl ChildTurnStart {
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn status(&self) -> ChildTurnStatus {
        self.status
    }

    pub fn completed(&self) -> bool {
        self.status == ChildTurnStatus::Completed
    }
}

#[derive(Debug)]
pub struct ChildAdapterError {
    message: String,
    capability_unavailable: bool,
    manual_handoff: Option<Box<ManualGoalHandoff>>,
}

impl ChildAdapterError {
    fn protocol(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            capability_unavailable: false,
            manual_handoff: None,
        }
    }

    fn capability(method: &str, error: &Value) -> Self {
        let code = error.get("code").and_then(Value::as_i64);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| message.len() <= 512 && !message.chars().any(char::is_control))
            .unwrap_or("provider rejected the request");
        let detail = code.map_or_else(
            || message.to_owned(),
            |code| format!("{message} (code {code})"),
        );
        Self {
            message: format!(
                "Codex App Server capability unavailable: {method}: {detail}; child remains blocked and no interactive fallback was executed"
            ),
            capability_unavailable: true,
            manual_handoff: None,
        }
    }

    fn with_manual_handoff(mut self, handoff: ManualGoalHandoff) -> Self {
        self.manual_handoff = Some(Box::new(handoff));
        self
    }

    pub fn manual_handoff(&self) -> Option<&ManualGoalHandoff> {
        self.manual_handoff.as_deref()
    }
}

impl fmt::Display for ChildAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ChildAdapterError {}

/// Reusable App Server boundary. Supplying a program is useful for controlled
/// evaluations; the default constructor uses the user's configured `codex`.
pub struct CodexChildAdapter {
    program: OsString,
}

impl CodexChildAdapter {
    pub fn from_environment() -> Self {
        Self {
            program: env::var_os("DRIFTCTL_CODEX_BIN").unwrap_or_else(|| "codex".into()),
        }
    }

    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn fork_and_migrate(
        &self,
        request: ChildForkRequest,
    ) -> Result<ChildMigration, ChildAdapterError> {
        let mut server = AppServer::start(&self.program, &request.worker_policy)?;
        let result = (|| {
            server.initialize()?;
            let parent_goal = server.get_goal(&request.parent_thread_id)?;
            let child = server.fork_persisted(
                &request.parent_thread_id,
                &request.isolated_cwd,
                &request.worker_policy,
                None,
            )?;
            server.set_worker_policy(&child.id, &child.cwd, &request.worker_policy)?;
            let initial_child_goal = match server.get_goal(&child.id) {
                Ok(goal) => goal,
                Err(error) if error.capability_unavailable => {
                    return Err(manual_goal_handoff_error(
                        &mut server,
                        error,
                        &request,
                        &parent_goal,
                        &child,
                        ManualGoalState::Unknown,
                    ));
                }
                Err(error) => return Err(error),
            };
            if initial_child_goal != GoalObservation::Absent
                && let Err(error) = server.clear_goal(&child.id)
            {
                return Err(manual_goal_handoff_error(
                    &mut server,
                    error,
                    &request,
                    &parent_goal,
                    &child,
                    ManualGoalState::Unknown,
                ));
            }
            if let Err(error) = server.set_goal(&child.id, &request.approved_objective) {
                return Err(manual_goal_handoff_error(
                    &mut server,
                    error,
                    &request,
                    &parent_goal,
                    &child,
                    ManualGoalState::Unknown,
                ));
            }
            let child_goal = match server.get_goal(&child.id) {
                Ok(goal) => goal,
                Err(error) => {
                    return Err(manual_goal_handoff_error(
                        &mut server,
                        error,
                        &request,
                        &parent_goal,
                        &child,
                        ManualGoalState::Unknown,
                    ));
                }
            };
            if child_goal.objective() != Some(request.approved_objective.as_str()) {
                return Err(manual_goal_handoff_error(
                    &mut server,
                    ChildAdapterError::protocol(
                        "child goal read-back does not exactly match the approved objective",
                    ),
                    &request,
                    &parent_goal,
                    &child,
                    child_goal.into(),
                ));
            }
            let parent_goal_after = server.get_goal(&request.parent_thread_id)?;
            if parent_goal_after != parent_goal {
                return Err(ChildAdapterError::protocol(
                    "parent goal changed while migrating the isolated child",
                ));
            }
            Ok(ChildMigration {
                parent_goal,
                child_id: child.id,
                child_cwd: child.cwd,
                child_goal,
            })
        })();
        server.stop();
        result
    }

    pub fn fork_preserving_goal(
        &self,
        request: PreservedForkRequest,
    ) -> Result<ChildMigration, ChildAdapterError> {
        let mut server = AppServer::start(&self.program, &request.worker_policy)?;
        let result = (|| {
            server.initialize()?;
            let parent_goal = server.get_goal(&request.parent_thread_id)?;
            let child = server.fork_persisted(
                &request.parent_thread_id,
                &request.isolated_cwd,
                &request.worker_policy,
                request.last_turn_id.as_deref(),
            )?;
            server.set_worker_policy(&child.id, &child.cwd, &request.worker_policy)?;
            let child_goal = server.get_goal(&child.id)?;
            if child_goal != parent_goal {
                return Err(ChildAdapterError::protocol(
                    "forked child inherited goal does not exactly match the parent",
                ));
            }
            let parent_goal_after = server.get_goal(&request.parent_thread_id)?;
            if parent_goal_after != parent_goal {
                return Err(ChildAdapterError::protocol(
                    "parent goal changed while creating a goal-preserving child",
                ));
            }
            Ok(ChildMigration {
                parent_goal,
                child_id: child.id,
                child_cwd: child.cwd,
                child_goal,
            })
        })();
        server.stop();
        result
    }

    pub fn start_child_turn(
        &self,
        request: ChildTurnRequest,
    ) -> Result<ChildTurnStart, ChildAdapterError> {
        let mut server = AppServer::start(&self.program, &request.worker_policy)?;
        let result = (|| {
            server.initialize()?;
            server.resume_child(&request)?;
            server.set_worker_policy(
                &request.child_thread_id,
                &request.child_cwd,
                &request.worker_policy,
            )?;
            server.start_turn(&request)
        })();
        server.stop();
        result
    }

    /// Re-read one persisted child goal through a fresh App Server process.
    pub fn observe_persisted_goal(
        &self,
        child_thread_id: &str,
    ) -> Result<GoalObservation, ChildAdapterError> {
        let child_thread_id = validate_identifier(child_thread_id.to_owned(), "child thread ID")?;
        let policy = WorkerPolicy::luna_max();
        let mut server = AppServer::start(&self.program, &policy)?;
        let result = (|| {
            server.initialize()?;
            server.get_goal(&child_thread_id)
        })();
        server.stop();
        result
    }
}

fn manual_goal_handoff_error(
    server: &mut AppServer,
    failure: ChildAdapterError,
    request: &ChildForkRequest,
    parent_goal: &GoalObservation,
    child: &ForkedChild,
    observed_goal: ManualGoalState,
) -> ChildAdapterError {
    match server.get_goal(&request.parent_thread_id) {
        Ok(parent_goal_after) if &parent_goal_after == parent_goal => {
            let message = failure.to_string();
            failure.with_manual_handoff(ManualGoalHandoff {
                child_id: child.id.clone(),
                child_cwd: child.cwd.clone(),
                observed_goal,
                intended_goal: request.approved_objective.clone(),
                failure: message,
            })
        }
        Ok(_) => ChildAdapterError::protocol(
            "parent goal changed during a failed isolated-child goal migration",
        ),
        Err(error) => ChildAdapterError::protocol(format!(
            "could not verify the parent goal after a failed isolated-child goal migration: {error}"
        )),
    }
}

struct ForkedChild {
    id: String,
    cwd: PathBuf,
}

struct AppServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: u64,
    notifications: Vec<Value>,
}

impl AppServer {
    fn start(program: &OsString, policy: &WorkerPolicy) -> Result<Self, ChildAdapterError> {
        let mut child = spawn_app_server(program, policy).map_err(|error| {
            ChildAdapterError::protocol(format!("could not launch Codex App Server: {error}"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ChildAdapterError::protocol("could not open Codex App Server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ChildAdapterError::protocol("could not open Codex App Server stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
            notifications: Vec::new(),
        })
    }

    fn initialize(&mut self) -> Result<(), ChildAdapterError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {"name":"driftctl", "title":null, "version":env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi":true, "requestAttestation":false},
            }),
        )?;
        self.notify("initialized", json!({}))
    }

    fn fork_persisted(
        &mut self,
        parent_thread_id: &str,
        isolated_cwd: &Path,
        worker_policy: &WorkerPolicy,
        last_turn_id: Option<&str>,
    ) -> Result<ForkedChild, ChildAdapterError> {
        let mut params = json!({
            "threadId": parent_thread_id,
            "cwd": isolated_cwd,
            "ephemeral": false,
            "model": worker_policy.model,
            "sandbox": "workspace-write",
            "approvalPolicy": "never",
        });
        if let Some(last_turn_id) = last_turn_id {
            params
                .as_object_mut()
                .expect("fork params are an object")
                .insert("lastTurnId".to_owned(), json!(last_turn_id));
        }
        let response = self.request("thread/fork", params)?;
        let thread = response.get("thread").ok_or_else(|| {
            ChildAdapterError::protocol("thread/fork response has no child thread")
        })?;
        let child_id = required_string(thread, "id", "thread/fork.result.thread")?;
        if child_id == parent_thread_id {
            return Err(ChildAdapterError::protocol(
                "thread/fork returned the parent instead of a distinct child",
            ));
        }
        if thread.get("ephemeral").and_then(Value::as_bool) != Some(false) {
            return Err(ChildAdapterError::protocol(
                "thread/fork did not return a persisted child; native goal migration is rejected",
            ));
        }
        let child_cwd = required_string(thread, "cwd", "thread/fork.result.thread")?;
        if child_cwd != isolated_cwd.to_string_lossy() {
            return Err(ChildAdapterError::protocol(
                "thread/fork child cwd does not exactly match the requested isolated cwd",
            ));
        }
        if thread.get("forkedFromId").and_then(Value::as_str) != Some(parent_thread_id) {
            return Err(ChildAdapterError::protocol(
                "thread/fork child lineage does not exactly match the requested parent",
            ));
        }
        Ok(ForkedChild {
            id: child_id,
            cwd: isolated_cwd.to_path_buf(),
        })
    }

    fn get_goal(&mut self, thread_id: &str) -> Result<GoalObservation, ChildAdapterError> {
        let response = self.request("thread/goal/get", json!({"threadId":thread_id}))?;
        let Some(goal) = response.get("goal") else {
            return Err(ChildAdapterError::protocol(
                "thread/goal/get response is missing goal",
            ));
        };
        if goal.is_null() {
            return Ok(GoalObservation::Absent);
        }
        let observed_thread_id = required_string(goal, "threadId", "thread/goal/get.result.goal")?;
        if observed_thread_id != thread_id {
            return Err(ChildAdapterError::protocol(
                "thread/goal/get returned a goal for a different child",
            ));
        }
        Ok(GoalObservation::Known(required_text(
            goal,
            "objective",
            "thread/goal/get.result.goal",
        )?))
    }

    fn resume_child(&mut self, request: &ChildTurnRequest) -> Result<(), ChildAdapterError> {
        self.resume_and_verify(
            &request.child_thread_id,
            &request.child_cwd,
            &request.worker_policy,
            false,
        )
    }

    fn resume_and_verify(
        &mut self,
        child_id: &str,
        child_cwd: &Path,
        policy: &WorkerPolicy,
        verify_policy: bool,
    ) -> Result<(), ChildAdapterError> {
        let response = self.request(
            "thread/resume",
            json!({
                "threadId":child_id,
                "model":policy.model,
                "sandbox":"workspace-write",
                "approvalPolicy":"never",
            }),
        )?;
        let thread = response.get("thread").ok_or_else(|| {
            ChildAdapterError::protocol("thread/resume response has no child thread")
        })?;
        let observed_child_id = required_string(thread, "id", "thread/resume.result.thread")?;
        let observed_child_cwd = required_string(thread, "cwd", "thread/resume.result.thread")?;
        if observed_child_id != child_id
            || observed_child_cwd != child_cwd.to_string_lossy()
            || thread.get("ephemeral").and_then(Value::as_bool) != Some(false)
        {
            return Err(ChildAdapterError::protocol(
                "thread/resume did not load the expected persisted child and cwd",
            ));
        }
        if verify_policy {
            let sandbox = response.pointer("/sandbox/type").and_then(Value::as_str);
            if response.get("model").and_then(Value::as_str) != Some(policy.model())
                || response.get("reasoningEffort").and_then(Value::as_str) != Some(policy.effort())
                || response.get("approvalPolicy").and_then(Value::as_str) != Some("never")
                || sandbox != Some("workspaceWrite")
                || response.get("cwd").and_then(Value::as_str)
                    != Some(child_cwd.to_string_lossy().as_ref())
            {
                return Err(ChildAdapterError::protocol(
                    "child worker policy read-back does not exactly match the requested policy",
                ));
            }
        }
        Ok(())
    }

    fn set_worker_policy(
        &mut self,
        child_id: &str,
        child_cwd: &Path,
        policy: &WorkerPolicy,
    ) -> Result<(), ChildAdapterError> {
        self.request(
            "thread/settings/update",
            json!({
                "threadId":child_id,
                "model":policy.model,
                "effort":policy.effort,
                "approvalPolicy":"never",
                "sandboxPolicy":{"type":"workspaceWrite"},
            }),
        )?;
        self.resume_and_verify(child_id, child_cwd, policy, true)
    }

    fn clear_goal(&mut self, child_id: &str) -> Result<(), ChildAdapterError> {
        let response = self.request("thread/goal/clear", json!({"threadId":child_id}))?;
        if response.get("cleared").and_then(Value::as_bool) != Some(true) {
            return Err(ChildAdapterError::protocol(
                "thread/goal/clear did not confirm a complete child goal clear",
            ));
        }
        Ok(())
    }

    fn set_goal(&mut self, child_id: &str, objective: &str) -> Result<(), ChildAdapterError> {
        let response = self.request(
            "thread/goal/set",
            json!({"threadId":child_id, "objective":objective}),
        )?;
        let goal = response.get("goal").ok_or_else(|| {
            ChildAdapterError::protocol("thread/goal/set response is missing child goal")
        })?;
        let observed_thread_id = required_string(goal, "threadId", "thread/goal/set.result.goal")?;
        let observed_objective = required_text(goal, "objective", "thread/goal/set.result.goal")?;
        if observed_thread_id != child_id || observed_objective != objective {
            return Err(ChildAdapterError::protocol(
                "thread/goal/set did not confirm the approved child goal",
            ));
        }
        Ok(())
    }

    fn start_turn(
        &mut self,
        request: &ChildTurnRequest,
    ) -> Result<ChildTurnStart, ChildAdapterError> {
        let response = self.request(
            "turn/start",
            json!({
                "threadId": request.child_thread_id,
                "input": [{"type":"text", "text":request.text()}],
                "model":request.worker_policy.model,
                "effort":request.worker_policy.effort,
                "approvalPolicy":"never",
                "sandboxPolicy":{"type":"workspaceWrite"},
            }),
        )?;
        let turn = response
            .get("turn")
            .ok_or_else(|| ChildAdapterError::protocol("turn/start response is missing turn"))?;
        let turn_id = required_string(turn, "id", "turn/start.result.turn")?;
        let status = parse_turn_status(turn.get("status"), "turn/start.result.turn.status")?;
        let status = self
            .notifications
            .iter()
            .filter_map(|notification| {
                terminal_notification(notification, &request.child_thread_id, &turn_id)
            })
            .next_back()
            .unwrap_or(status);
        let status = if status == ChildTurnStatus::InProgress {
            self.wait_for_terminal_turn(&request.child_thread_id, &turn_id)?
        } else {
            status
        };
        Ok(ChildTurnStart { turn_id, status })
    }

    fn wait_for_terminal_turn(
        &mut self,
        child_id: &str,
        turn_id: &str,
    ) -> Result<ChildTurnStatus, ChildAdapterError> {
        loop {
            let message = self.read_json_line()?;
            if let Some(status) = terminal_notification(&message, child_id, turn_id) {
                return Ok(status);
            }
            let Some(object) = message.as_object() else {
                return Err(ChildAdapterError::protocol(
                    "Codex App Server emitted a non-object turn message",
                ));
            };
            if object.get("method").is_some() && object.get("id").is_none() {
                continue;
            }
            if object.get("method").is_some() && object.get("id").is_some() {
                return Err(ChildAdapterError::protocol(
                    "child turn requires approval or user input; child remains blocked and Driftctl did not auto-approve it",
                ));
            }
            return Err(ChildAdapterError::protocol(
                "Codex App Server emitted an unexpected message while the child turn was running",
            ));
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, ChildAdapterError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_json(&json!({"id":id, "method":method, "params":params}))?;
        loop {
            let response = self.read_json_line()?;
            let object = response.as_object().ok_or_else(|| {
                ChildAdapterError::protocol(
                    "Codex App Server emitted a non-object protocol message",
                )
            })?;
            if object.get("method").is_some() && object.get("id").is_none() {
                self.notifications.push(response);
                continue;
            }
            if object.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(ChildAdapterError::protocol(
                    "Codex App Server returned an unexpected response ID",
                ));
            }
            if let Some(error) = object.get("error") {
                return Err(ChildAdapterError::capability(method, error));
            }
            return object.get("result").cloned().ok_or_else(|| {
                ChildAdapterError::protocol(format!(
                    "Codex App Server {method} response has no result"
                ))
            });
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ChildAdapterError> {
        self.write_json(&json!({"method":method, "params":params}))
    }

    fn write_json(&mut self, value: &Value) -> Result<(), ChildAdapterError> {
        let message = serde_json::to_vec(value).map_err(|error| {
            ChildAdapterError::protocol(format!("could not encode Codex request: {error}"))
        })?;
        self.stdin
            .write_all(&message)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|error| {
                ChildAdapterError::protocol(format!("could not write to Codex App Server: {error}"))
            })
    }

    fn read_json_line(&mut self) -> Result<Value, ChildAdapterError> {
        let mut line = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            let read = self.stdout.read(&mut byte).map_err(|error| {
                ChildAdapterError::protocol(format!(
                    "could not read Codex App Server response: {error}"
                ))
            })?;
            if read == 0 {
                return Err(ChildAdapterError::protocol(if line.is_empty() {
                    "Codex App Server closed before a response"
                } else {
                    "Codex App Server response is truncated"
                }));
            }
            if byte[0] == b'\n' {
                break;
            }
            if line.len() == MAX_PROTOCOL_LINE_BYTES {
                return Err(ChildAdapterError::protocol(
                    "Codex App Server response exceeds the size limit",
                ));
            }
            line.push(byte[0]);
        }
        serde_json::from_slice(&line)
            .map_err(|_| ChildAdapterError::protocol("Codex App Server emitted malformed JSON"))
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_app_server(program: &OsString, policy: &WorkerPolicy) -> std::io::Result<Child> {
    const MAX_ATTEMPTS: usize = 3;
    let effort_override = format!("model_reasoning_effort={}", json!(policy.effort()));
    for attempt in 1..=MAX_ATTEMPTS {
        let result = Command::new(program)
            .args(["-c", &effort_override, "app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Err(error) if attempt < MAX_ATTEMPTS && error.raw_os_error() == Some(26) => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
    unreachable!("bounded App Server launch loop always returns")
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn terminal_notification(
    notification: &Value,
    child_id: &str,
    turn_id: &str,
) -> Option<ChildTurnStatus> {
    if notification.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }
    let params = notification.get("params")?;
    if params.get("threadId").and_then(Value::as_str) != Some(child_id) {
        return None;
    }
    let turn = params.get("turn")?;
    if turn.get("id").and_then(Value::as_str) != Some(turn_id) {
        return None;
    }
    parse_turn_status(turn.get("status"), "turn/completed.params.turn.status").ok()
}

fn parse_turn_status(
    value: Option<&Value>,
    location: &str,
) -> Result<ChildTurnStatus, ChildAdapterError> {
    match value.and_then(Value::as_str) {
        Some("inProgress") => Ok(ChildTurnStatus::InProgress),
        Some("completed") => Ok(ChildTurnStatus::Completed),
        Some("failed") => Ok(ChildTurnStatus::Failed),
        Some("interrupted") => Ok(ChildTurnStatus::Interrupted),
        _ => Err(ChildAdapterError::protocol(format!(
            "{location} is invalid"
        ))),
    }
}

fn canonical_child_cwd(path: &Path) -> Result<PathBuf, ChildAdapterError> {
    let canonical = path.canonicalize().map_err(|error| {
        ChildAdapterError::protocol(format!(
            "could not canonicalize isolated child cwd: {error}"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(ChildAdapterError::protocol(
            "isolated child cwd must be an existing directory",
        ));
    }
    Ok(canonical)
}

fn validate_identifier(value: String, label: &str) -> Result<String, ChildAdapterError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(ChildAdapterError::protocol(format!("invalid {label}")));
    }
    Ok(value)
}

fn validate_objective(value: String) -> Result<String, ChildAdapterError> {
    validate_text(value, "approved objective")
}

fn validate_text(value: String, label: &str) -> Result<String, ChildAdapterError> {
    if value.is_empty() || value.len() > MAX_PROTOCOL_LINE_BYTES || value.contains('\0') {
        return Err(ChildAdapterError::protocol(format!("invalid {label}")));
    }
    Ok(value)
}

fn required_string(
    value: &Value,
    field: &str,
    location: &str,
) -> Result<String, ChildAdapterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 16 * 1024 && !value.chars().any(char::is_control)
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ChildAdapterError::protocol(format!("{location}.{field} must be a non-empty string"))
        })
}

fn required_text(value: &Value, field: &str, location: &str) -> Result<String, ChildAdapterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= MAX_PROTOCOL_LINE_BYTES && !value.contains('\0')
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ChildAdapterError::protocol(format!("{location}.{field} must be non-empty text"))
        })
}
