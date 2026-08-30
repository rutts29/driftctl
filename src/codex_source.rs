use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::intent_history::{SourceProvider, SourceRole};
use crate::session_bundle::{BundleRecord, NativeGoal, NeutralSessionBundle};

const MAX_PROTOCOL_LINE_BYTES: usize = 1024 * 1024;
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
    user_records: Vec<ImportedUserRecord>,
}

impl ImportedSession {
    pub(crate) fn imported_user_record_count(&self) -> usize {
        self.user_records.len()
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
            .user_records
            .iter()
            .map(|record| BundleRecord::new(&record.id, SourceRole::User, &record.text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SourceError::new(format!("invalid imported Codex record: {error}")))?;
        NeutralSessionBundle::from_records_with_native_goal(
            SourceProvider::Codex,
            &self.session_id,
            &self.repository_digest,
            // GoalController observation is not integrated at the App Server
            // intake boundary yet, so absence must not be inferred.
            NativeGoal::Unknown,
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
            .field("user_record_count", &self.user_records.len())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct ImportedUserRecord {
    id: String,
    text: String,
}

impl fmt::Debug for ImportedUserRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportedUserRecord")
            .field("id", &self.id)
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
        let imported = parse_imported_session(thread, canonical_root, &selected_id)?;
        // Convert at the provider boundary. This validates the exact neutral
        // handoff without classifying intent or changing public inspect output.
        imported.neutral_bundle()?;
        Ok(imported)
    })();
    server.stop();
    result
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
        loop {
            let mut byte = [0_u8; 1];
            let read = self.stdout.read(&mut byte).map_err(|error| {
                SourceError::new(format!("could not read Codex App Server response: {error}"))
            })?;
            if read == 0 {
                return Err(SourceError::new(if line.is_empty() {
                    "Codex App Server closed before a response"
                } else {
                    "Codex App Server response is truncated"
                }));
            }
            if byte[0] == b'\n' {
                break;
            }
            if line.len() == MAX_PROTOCOL_LINE_BYTES {
                return Err(SourceError::new(
                    "Codex App Server response exceeds the size limit",
                ));
            }
            line.push(byte[0]);
        }
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
    let mut user_records = Vec::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        let turn_location = format!("thread/read.result.thread.turns[{turn_index}]");
        let items = required_array(turn, "items", &turn_location)?;
        for (item_index, item) in items.iter().enumerate() {
            let item_location = format!("{turn_location}.items[{item_index}]");
            if item.get("type").and_then(Value::as_str) != Some("userMessage") {
                continue;
            }
            let id = required_string(item, "id", &item_location)?;
            let content = required_array(item, "content", &item_location)?;
            for (content_index, part) in content.iter().enumerate() {
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let text_location = format!("{item_location}.content[{content_index}]");
                let text = required_text(part, "text", &text_location)?;
                user_records.push(ImportedUserRecord {
                    id: format!("{id}:{content_index}"),
                    text,
                });
            }
        }
    }
    Ok(ImportedSession {
        session_id: id,
        repository_digest: repository_digest(canonical_root),
        user_records,
    })
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

fn optional_i64(value: &Value, field: &str, location: &str) -> Result<Option<i64>, SourceError> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            SourceError::new(format!("{location}.{field} must be an integer or null"))
        }),
        None => Ok(None),
    }
}
