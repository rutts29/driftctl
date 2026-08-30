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

use serde_json::{Value, json};

const MAX_PROTOCOL_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoalObservation {
    Absent,
    Known(String),
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

impl ChildForkRequest {
    pub fn new(
        parent_thread_id: impl Into<String>,
        isolated_cwd: impl AsRef<Path>,
        approved_objective: impl Into<String>,
    ) -> Result<Self, ChildAdapterError> {
        let parent_thread_id = validate_identifier(parent_thread_id.into(), "parent thread ID")?;
        let isolated_cwd = isolated_cwd.as_ref().canonicalize().map_err(|error| {
            ChildAdapterError::protocol(format!(
                "could not canonicalize isolated child cwd: {error}"
            ))
        })?;
        if !isolated_cwd.is_dir() {
            return Err(ChildAdapterError::protocol(
                "isolated child cwd must be an existing directory",
            ));
        }
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
pub struct ChildAdapterError(String);

impl ChildAdapterError {
    fn protocol(message: impl Into<String>) -> Self {
        Self(message.into())
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
        Self(format!(
            "Codex App Server capability unavailable: {method}: {detail}; child remains blocked and no interactive fallback was executed"
        ))
    }
}

impl fmt::Display for ChildAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
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
        let mut server = AppServer::start(&self.program)?;
        let result = (|| {
            server.initialize()?;
            let parent_goal = server.get_goal(&request.parent_thread_id)?;
            let child = server.fork_persisted(&request)?;
            server.set_worker_policy(&child.id, &child.cwd, &request.worker_policy)?;
            let initial_child_goal = server.get_goal(&child.id)?;
            if initial_child_goal != GoalObservation::Absent {
                server.clear_goal(&child.id)?;
            }
            server.set_goal(&child.id, &request.approved_objective)?;
            let child_goal = server.get_goal(&child.id)?;
            if child_goal.objective() != Some(request.approved_objective.as_str()) {
                return Err(ChildAdapterError::protocol(
                    "child goal read-back does not exactly match the approved objective",
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

    pub fn start_child_turn(
        &self,
        request: ChildTurnRequest,
    ) -> Result<ChildTurnStart, ChildAdapterError> {
        let mut server = AppServer::start(&self.program)?;
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
    fn start(program: &OsString) -> Result<Self, ChildAdapterError> {
        let mut child = Command::new(program)
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
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
                "capabilities": {"experimentalApi":false, "requestAttestation":false},
            }),
        )?;
        self.notify("initialized", json!({}))
    }

    fn fork_persisted(
        &mut self,
        request: &ChildForkRequest,
    ) -> Result<ForkedChild, ChildAdapterError> {
        let response = self.request(
            "thread/fork",
            json!({
                "threadId": request.parent_thread_id,
                "cwd": request.isolated_cwd,
                "ephemeral": false,
                "model": request.worker_policy.model,
                "sandbox": "workspace-write",
                "approvalPolicy": "never",
            }),
        )?;
        let thread = response.get("thread").ok_or_else(|| {
            ChildAdapterError::protocol("thread/fork response has no child thread")
        })?;
        let child_id = required_string(thread, "id", "thread/fork.result.thread")?;
        if child_id == request.parent_thread_id {
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
        if child_cwd != request.isolated_cwd.to_string_lossy() {
            return Err(ChildAdapterError::protocol(
                "thread/fork child cwd does not exactly match the requested isolated cwd",
            ));
        }
        Ok(ForkedChild {
            id: child_id,
            cwd: request.isolated_cwd.clone(),
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
        let response = self.request(
            "thread/resume",
            json!({
                "threadId":request.child_thread_id,
                "model":request.worker_policy.model,
                "sandbox":"workspace-write",
                "approvalPolicy":"never",
            }),
        )?;
        let thread = response.get("thread").ok_or_else(|| {
            ChildAdapterError::protocol("thread/resume response has no child thread")
        })?;
        let child_id = required_string(thread, "id", "thread/resume.result.thread")?;
        let child_cwd = required_string(thread, "cwd", "thread/resume.result.thread")?;
        if child_id != request.child_thread_id
            || child_cwd != request.child_cwd.to_string_lossy()
            || thread.get("ephemeral").and_then(Value::as_bool) != Some(false)
        {
            return Err(ChildAdapterError::protocol(
                "thread/resume did not load the expected persisted child and cwd",
            ));
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
        self.request(
            "thread/read",
            json!({"threadId":child_id,"includeTurns":false}),
        )?;
        let notification = self
            .notifications
            .iter()
            .rev()
            .find(|notification| {
                notification.get("method").and_then(Value::as_str)
                    == Some("thread/settings/updated")
                    && notification
                        .pointer("/params/threadId")
                        .and_then(Value::as_str)
                        == Some(child_id)
            })
            .ok_or_else(|| {
                ChildAdapterError::protocol(
                    "child worker policy update had no verifiable settings notification",
                )
            })?;
        let settings = notification
            .pointer("/params/threadSettings")
            .ok_or_else(|| ChildAdapterError::protocol("child worker settings are missing"))?;
        let observed_cwd = settings.get("cwd").and_then(Value::as_str);
        let sandbox = settings
            .pointer("/sandboxPolicy/type")
            .and_then(Value::as_str);
        if settings.get("model").and_then(Value::as_str) != Some(policy.model())
            || settings.get("effort").and_then(Value::as_str) != Some(policy.effort())
            || settings.get("approvalPolicy").and_then(Value::as_str) != Some("never")
            || sandbox != Some("workspaceWrite")
            || observed_cwd != Some(child_cwd.to_string_lossy().as_ref())
        {
            return Err(ChildAdapterError::protocol(
                "child worker policy read-back does not exactly match the requested policy",
            ));
        }
        Ok(())
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
