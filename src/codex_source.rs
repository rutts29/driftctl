use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::intent_history::{SourceProvider, SourceRole};
use crate::session_bundle::{BundleRecord, NativeGoal, NeutralSessionBundle};

const MAX_PROTOCOL_LINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_THREAD_LIST_PAGES: usize = 100;
const THREAD_SOURCE_KINDS: [&str; 10] = [
    "cli",
    "vscode",
    "exec",
    "appServer",
    "subAgent",
    "subAgentReview",
    "subAgentCompact",
    "subAgentThreadSpawn",
    "subAgentOther",
    "unknown",
];

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ImportedSession {
    session_id: String,
    repository_digest: String,
    native_goal: NativeGoal,
    thread_snapshot: Value,
    records: Vec<ImportedRecord>,
}

impl ImportedSession {
    pub(crate) fn imported_user_record_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| matches!(record.role, SourceRole::User))
            .count()
    }

    pub(crate) fn redacted_session(&self) -> String {
        let digest = Sha256::digest(self.session_id.as_bytes());
        let opaque: String = digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("codex-session:sha256:{opaque}")
    }

    pub(crate) fn source_digest(&self) -> String {
        self.neutral_bundle()
            .expect("parsed Codex user records must form a valid neutral bundle")
            .source()
            .digest()
            .to_owned()
    }

    /// Converts the App Server's explicit user messages to strict neutral
    /// records before any semantic resolver can inspect them.
    pub(crate) fn neutral_bundle(&self) -> Result<NeutralSessionBundle, SourceError> {
        let records = self
            .records
            .iter()
            .map(|record| BundleRecord::new(&record.id, record.role.clone(), &record.content))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SourceError::new(format!("invalid imported Codex record: {error}")))?;
        NeutralSessionBundle::from_records_with_native_goal(
            SourceProvider::Codex,
            &self.session_id,
            &self.repository_digest,
            self.native_goal.clone(),
            records,
        )
        .map_err(|error| SourceError::new(format!("invalid imported Codex bundle: {error}")))
    }
}

impl fmt::Debug for ImportedSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportedSession")
            .field("redacted_session", &self.redacted_session())
            .field("repository_digest", &self.repository_digest)
            .field("record_count", &self.records.len())
            .field("user_record_count", &self.imported_user_record_count())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct ImportedRecord {
    id: String,
    role: SourceRole,
    content: String,
}

impl fmt::Debug for ImportedRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportedRecord")
            .field("id", &self.id)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionSelection<'a> {
    Last,
    Explicit(&'a str),
}

#[derive(Debug)]
pub(crate) struct SourceError(String);

impl SourceError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

pub(crate) fn inspect(
    root: &Path,
    selection: SessionSelection<'_>,
) -> Result<ImportedSession, SourceError> {
    let canonical_root = root.canonicalize().map_err(|error| {
        SourceError::new(format!("could not canonicalize current directory: {error}"))
    })?;
    let canonical_root = canonical_root
        .to_str()
        .ok_or_else(|| SourceError::new("current directory is not valid UTF-8"))?;
    let mut server = AppServer::start()?;
    let result = (|| {
        server.initialize()?;
        let selected_id = match selection {
            SessionSelection::Last => server.latest_thread_id(canonical_root)?,
            SessionSelection::Explicit(id) => validate_explicit_session_id(id)?,
        };
        let thread = server.read_thread(&selected_id)?;
        let native_goal = server.observe_goal(&selected_id)?;
        let imported = parse_imported_session(thread, canonical_root, &selected_id, native_goal)?;
        // Convert at the provider boundary. This validates the exact neutral
        // handoff without classifying intent or changing public inspect output.
        imported.neutral_bundle()?;
        Ok(imported)
    })();
    server.stop();
    result
}

/// Re-import the exact selected parent after a paid resolver call and reject
/// a stale result without exposing the private session locator.
pub(crate) fn verify_unchanged(root: &Path, expected: &ImportedSession) -> Result<(), SourceError> {
    let observed = inspect(root, SessionSelection::Explicit(&expected.session_id))?;
    if &observed == expected {
        Ok(())
    } else {
        Err(SourceError::new(
            "Codex source session or native goal changed during inspect",
        ))
    }
}

fn validate_explicit_session_id(id: &str) -> Result<String, SourceError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(SourceError::new("invalid explicit Codex session ID"));
    }
    Ok(id.to_owned())
}

struct AppServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: u64,
}

impl AppServer {
    fn start() -> Result<Self, SourceError> {
        let program = env::var_os("DRIFTCTL_CODEX_BIN").unwrap_or_else(|| "codex".into());
        let mut child = Command::new(program)
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                SourceError::new(format!("could not launch Codex App Server: {error}"))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SourceError::new("could not open Codex App Server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SourceError::new("could not open Codex App Server stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
        })
    }

    fn initialize(&mut self) -> Result<(), SourceError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "driftctl",
                    "title": null,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": false,
                    "requestAttestation": false,
                },
            }),
        )?;
        self.notify("initialized", json!({}))
    }

    fn latest_thread_id(&mut self, canonical_root: &str) -> Result<String, SourceError> {
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut best: Option<ThreadCandidate> = None;
        let mut tied_best = false;
        let mut exhausted = false;

        for _ in 0..MAX_THREAD_LIST_PAGES {
            let response = self.request(
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": 100,
                    "sortKey": "recency_at",
                    "sortDirection": "desc",
                    "sourceKinds": THREAD_SOURCE_KINDS,
                    "cwd": canonical_root,
                    "useStateDbOnly": true,
                }),
            )?;
            let (candidates, next_cursor) = parse_thread_page(response, canonical_root)?;
            for candidate in candidates {
                match &best {
                    None => best = Some(candidate),
                    Some(current) if candidate.recency > current.recency => {
                        best = Some(candidate);
                        tied_best = false;
                    }
                    Some(current)
                        if candidate.recency == current.recency && candidate.id != current.id =>
                    {
                        tied_best = true;
                    }
                    _ => {}
                }
            }
            let Some(next_cursor) = next_cursor else {
                exhausted = true;
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(SourceError::new(
                    "Codex App Server returned a repeated pagination cursor",
                ));
            }
            cursor = Some(next_cursor);
        }

        if !exhausted {
            return Err(SourceError::new(
                "Codex App Server thread listing exceeds the page limit",
            ));
        }
        if tied_best {
            return Err(SourceError::new(
                "latest Codex session is ambiguous for the current repository",
            ));
        }
        best.map(|candidate| candidate.id)
            .ok_or_else(|| SourceError::new("no Codex session matches the current repository"))
    }

    fn read_thread(&mut self, thread_id: &str) -> Result<Value, SourceError> {
        self.request(
            "thread/read",
            json!({
                "threadId": thread_id,
                "includeTurns": true,
            }),
        )
    }

    fn observe_goal(&mut self, thread_id: &str) -> Result<NativeGoal, SourceError> {
        let response = match self.request(
            "thread/goal/get",
            json!({
                "threadId": thread_id,
            }),
        ) {
            Ok(response) => response,
            Err(error) if error.to_string() == "Codex App Server rejected thread/goal/get" => {
                return Ok(NativeGoal::Unknown);
            }
            Err(error) => return Err(error),
        };
        parse_native_goal(response, thread_id)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, SourceError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_json(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;

        loop {
            let response = self.read_json_line()?;
            let object = response.as_object().ok_or_else(|| {
                SourceError::new("Codex App Server emitted a non-object protocol message")
            })?;
            if object.get("method").is_some() && object.get("id").is_none() {
                continue;
            }
            let response_id = object.get("id").and_then(Value::as_u64).ok_or_else(|| {
                SourceError::new("Codex App Server response is missing a numeric ID")
            })?;
            if response_id != id {
                return Err(SourceError::new(
                    "Codex App Server returned an unexpected response ID",
                ));
            }
            if object.get("error").is_some() {
                return Err(SourceError::new(format!(
                    "Codex App Server rejected {method}"
                )));
            }
            return object.get("result").cloned().ok_or_else(|| {
                SourceError::new(format!("Codex App Server {method} response has no result"))
            });
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), SourceError> {
        self.write_json(&json!({
            "method": method,
            "params": params,
        }))
    }

    fn write_json(&mut self, value: &Value) -> Result<(), SourceError> {
        let message = serde_json::to_vec(value).map_err(|error| {
            SourceError::new(format!(
                "could not encode Codex App Server request: {error}"
            ))
        })?;
        self.stdin
            .write_all(&message)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|error| {
                SourceError::new(format!("could not write to Codex App Server: {error}"))
            })
    }

    fn read_json_line(&mut self) -> Result<Value, SourceError> {
        let mut line = Vec::new();
        let read = (&mut self.stdout)
            .take((MAX_PROTOCOL_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|error| {
                SourceError::new(format!("could not read Codex App Server response: {error}"))
            })?;
        if read == 0 {
            return Err(SourceError::new(
                "Codex App Server closed before a response",
            ));
        }
        if line.last() != Some(&b'\n') {
            return Err(SourceError::new(if line.len() > MAX_PROTOCOL_LINE_BYTES {
                "Codex App Server response exceeds the 64 MiB size limit"
            } else {
                "Codex App Server response is truncated"
            }));
        }
        line.pop();
        serde_json::from_slice(&line)
            .map_err(|_| SourceError::new("Codex App Server emitted malformed JSON"))
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct ThreadCandidate {
    id: String,
    recency: i64,
}

fn parse_thread_page(
    response: Value,
    canonical_root: &str,
) -> Result<(Vec<ThreadCandidate>, Option<String>), SourceError> {
    let data = required_array(&response, "data", "thread/list.result")?;
    let mut candidates = Vec::new();
    for (index, thread) in data.iter().enumerate() {
        let location = format!("thread/list.result.data[{index}]");
        let id = required_string(thread, "id", &location)?;
        let cwd = required_string(thread, "cwd", &location)?;
        if cwd != canonical_root {
            continue;
        }
        let recency = optional_i64(thread, "recencyAt", &location)?
            .or(optional_i64(thread, "updatedAt", &location)?)
            .ok_or_else(|| {
                SourceError::new(format!("{location}.recencyAt must be a number or null"))
            })?;
        candidates.push(ThreadCandidate { id, recency });
    }
    let next_cursor = match response.get("nextCursor") {
        Some(Value::Null) => None,
        Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 4096 => {
            Some(cursor.clone())
        }
        Some(_) => {
            return Err(SourceError::new(
                "thread/list.result.nextCursor must be null or a non-empty string",
            ));
        }
        None => return Err(SourceError::new("thread/list.result.nextCursor is missing")),
    };
    Ok((candidates, next_cursor))
}

fn parse_imported_session(
    response: Value,
    canonical_root: &str,
    requested_id: &str,
    native_goal: NativeGoal,
) -> Result<ImportedSession, SourceError> {
    let thread = response
        .get("thread")
        .ok_or_else(|| SourceError::new("thread/read.result.thread is missing"))?;
    let id = required_string(thread, "id", "thread/read.result.thread")?;
    if id != requested_id {
        return Err(SourceError::new("thread/read returned a different session"));
    }
    let cwd = required_string(thread, "cwd", "thread/read.result.thread")?;
    if cwd != canonical_root {
        return Err(SourceError::new(
            "Codex session does not belong to the current repository",
        ));
    }
    let turns = required_array(thread, "turns", "thread/read.result.thread")?;
    let mut records = Vec::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        let turn_location = format!("thread/read.result.thread.turns[{turn_index}]");
        let items = required_array(turn, "items", &turn_location)?;
        for (item_index, item) in items.iter().enumerate() {
            let item_location = format!("{turn_location}.items[{item_index}]");
            let provider_type = required_string(item, "type", &item_location)?;
            if provider_type == "userMessage" {
                parse_user_message(item, &item_location, &mut records)?;
                continue;
            }
            let role = provider_item_role(&provider_type).ok_or_else(|| {
                SourceError::new(format!(
                    "{item_location}.type is an unsupported Codex thread item"
                ))
            })?;
            validate_provider_item(item, &provider_type, &item_location)?;
            let id = required_string(item, "id", &item_location)?;
            records.push(ImportedRecord {
                id,
                role,
                content: opaque_provider_evidence(item, &provider_type)?,
            });
        }
    }
    Ok(ImportedSession {
        session_id: id,
        repository_digest: repository_digest(canonical_root),
        native_goal,
        thread_snapshot: thread.clone(),
        records,
    })
}

fn parse_user_message(
    item: &Value,
    item_location: &str,
    records: &mut Vec<ImportedRecord>,
) -> Result<(), SourceError> {
    let id = required_string(item, "id", item_location)?;
    let content = required_array(item, "content", item_location)?;
    for (content_index, part) in content.iter().enumerate() {
        let part_location = format!("{item_location}.content[{content_index}]");
        let provider_type = required_string(part, "type", &part_location)?;
        let (role, content) = if provider_type == "text" {
            (
                SourceRole::User,
                required_text(part, "text", &part_location)?,
            )
        } else {
            validate_user_attachment(part, &provider_type, &part_location)?;
            (
                SourceRole::SystemObservation,
                opaque_provider_evidence(part, &format!("userInput.{provider_type}"))?,
            )
        };
        records.push(ImportedRecord {
            id: format!("{id}:{content_index}"),
            role,
            content,
        });
    }
    Ok(())
}

fn provider_item_role(provider_type: &str) -> Option<SourceRole> {
    match provider_type {
        "agentMessage" | "plan" | "reasoning" => Some(SourceRole::Assistant),
        "commandExecution"
        | "fileChange"
        | "mcpToolCall"
        | "dynamicToolCall"
        | "collabAgentToolCall"
        | "webSearch"
        | "imageView"
        | "sleep"
        | "imageGeneration" => Some(SourceRole::Tool),
        "hookPrompt" | "subAgentActivity" | "enteredReviewMode" | "exitedReviewMode"
        | "contextCompaction" => Some(SourceRole::SystemObservation),
        _ => None,
    }
}

fn validate_provider_item(
    item: &Value,
    provider_type: &str,
    location: &str,
) -> Result<(), SourceError> {
    required_string(item, "id", location)?;
    match provider_type {
        "agentMessage" | "plan" => {
            required_provider_text(item, "text", location)?;
        }
        "reasoning" | "contextCompaction" => {}
        "hookPrompt" => {
            required_array(item, "fragments", location)?;
        }
        "commandExecution" => {
            required_provider_text(item, "command", location)?;
            required_array(item, "commandActions", location)?;
            required_string(item, "cwd", location)?;
            required_string(item, "status", location)?;
        }
        "fileChange" => {
            required_array(item, "changes", location)?;
            required_string(item, "status", location)?;
        }
        "mcpToolCall" => {
            required_field(item, "arguments", location)?;
            required_string(item, "server", location)?;
            required_string(item, "tool", location)?;
            required_string(item, "status", location)?;
        }
        "dynamicToolCall" => {
            required_field(item, "arguments", location)?;
            required_string(item, "tool", location)?;
            required_string(item, "status", location)?;
        }
        "collabAgentToolCall" => {
            required_object(item, "agentsStates", location)?;
            required_string_array(item, "receiverThreadIds", location)?;
            required_string(item, "senderThreadId", location)?;
            required_string(item, "status", location)?;
            required_string(item, "tool", location)?;
        }
        "subAgentActivity" => {
            required_string(item, "agentPath", location)?;
            required_string(item, "agentThreadId", location)?;
            required_string(item, "kind", location)?;
        }
        "webSearch" => {
            required_provider_text(item, "query", location)?;
        }
        "imageView" => {
            required_string(item, "path", location)?;
        }
        "sleep" => {
            required_u64(item, "durationMs", location)?;
        }
        "imageGeneration" => {
            required_provider_text(item, "result", location)?;
            required_string(item, "status", location)?;
        }
        "enteredReviewMode" | "exitedReviewMode" => {
            required_provider_text(item, "review", location)?;
        }
        _ => unreachable!("provider role validation precedes item validation"),
    }
    Ok(())
}

fn validate_user_attachment(
    part: &Value,
    provider_type: &str,
    location: &str,
) -> Result<(), SourceError> {
    match provider_type {
        "image" | "audio" => {
            required_provider_text(part, "url", location)?;
        }
        "localImage" | "localAudio" => {
            required_string(part, "path", location)?;
        }
        "skill" | "mention" => {
            required_string(part, "name", location)?;
            required_string(part, "path", location)?;
        }
        _ => {
            return Err(SourceError::new(format!(
                "{location}.type is an unsupported Codex user input"
            )));
        }
    }
    Ok(())
}

fn opaque_provider_evidence(item: &Value, provider_type: &str) -> Result<String, SourceError> {
    let bytes = serde_json::to_vec(item)
        .map_err(|_| SourceError::new("could not normalize a Codex provider item"))?;
    let mut evidence = json!({
        "provider_type": provider_type,
        "item_bytes": bytes.len(),
        "item_digest": format!("sha256:{:x}", Sha256::digest(&bytes)),
    });
    if provider_type == "contextCompaction" {
        evidence
            .as_object_mut()
            .expect("constructed object")
            .insert("compaction_boundary".to_owned(), Value::Bool(true));
    }
    serde_json::to_string(&evidence)
        .map_err(|_| SourceError::new("could not normalize Codex provider evidence"))
}

fn parse_native_goal(response: Value, requested_id: &str) -> Result<NativeGoal, SourceError> {
    let Some(goal) = response.get("goal") else {
        return Err(SourceError::new("thread/goal/get.result.goal is missing"));
    };
    if goal.is_null() {
        return Ok(NativeGoal::Absent);
    }
    let thread_id = required_string(goal, "threadId", "thread/goal/get.result.goal")?;
    if thread_id != requested_id {
        return Err(SourceError::new(
            "thread/goal/get returned a goal for a different session",
        ));
    }
    let objective = required_text(goal, "objective", "thread/goal/get.result.goal")?;
    NativeGoal::known(objective)
        .map_err(|error| SourceError::new(format!("invalid Codex native goal: {error}")))
}

fn repository_digest(canonical_root: &str) -> String {
    let digest = Sha256::digest(canonical_root.as_bytes());
    format!("sha256:{digest:x}")
}

fn required_array<'a>(
    value: &'a Value,
    field: &str,
    location: &str,
) -> Result<&'a [Value], SourceError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| SourceError::new(format!("{location}.{field} must be an array")))
}

fn required_object<'a>(
    value: &'a Value,
    field: &str,
    location: &str,
) -> Result<&'a serde_json::Map<String, Value>, SourceError> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| SourceError::new(format!("{location}.{field} must be an object")))
}

fn required_field<'a>(
    value: &'a Value,
    field: &str,
    location: &str,
) -> Result<&'a Value, SourceError> {
    value
        .get(field)
        .ok_or_else(|| SourceError::new(format!("{location}.{field} is missing")))
}

fn required_string_array(value: &Value, field: &str, location: &str) -> Result<(), SourceError> {
    let values = required_array(value, field, location)?;
    if values.iter().all(Value::is_string) {
        Ok(())
    } else {
        Err(SourceError::new(format!(
            "{location}.{field} must contain only strings"
        )))
    }
}

fn required_u64(value: &Value, field: &str, location: &str) -> Result<u64, SourceError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| SourceError::new(format!("{location}.{field} must be an unsigned integer")))
}

fn required_string(value: &Value, field: &str, location: &str) -> Result<String, SourceError> {
    let result = value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 16 * 1024 && !value.chars().any(char::is_control)
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            SourceError::new(format!("{location}.{field} must be a non-empty string"))
        })?;
    Ok(result)
}

fn required_text(value: &Value, field: &str, location: &str) -> Result<String, SourceError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 1024 * 1024 && !value.contains('\0'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            SourceError::new(format!(
                "{location}.{field} must be a non-empty text string"
            ))
        })
}

fn required_provider_text(
    value: &Value,
    field: &str,
    location: &str,
) -> Result<String, SourceError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 1024 * 1024 && !value.contains('\0'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| SourceError::new(format!("{location}.{field} must be a text string")))
}

fn optional_i64(value: &Value, field: &str, location: &str) -> Result<Option<i64>, SourceError> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            SourceError::new(format!("{location}.{field} must be an integer or null"))
        }),
        None => Ok(None),
    }
}
