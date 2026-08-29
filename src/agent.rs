use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use serde_json::Value;

use crate::Snapshot;

const MAX_TRAJECTORY_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRAJECTORY_EVENTS: usize = 100_000;
const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalEvent {
    Completed,
    Failed,
}

#[derive(Debug)]
struct CapturedStream {
    bytes: Vec<u8>,
    exceeded: bool,
}

#[derive(Debug)]
struct ProcessOutput {
    status: ExitStatus,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

#[derive(Debug)]
struct ProtectedFile {
    label: String,
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    readonly: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentError(String);

impl AgentError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AgentError {}

pub(crate) fn run_codex(root: &Path, snapshot: &Snapshot) -> Result<PathBuf, AgentError> {
    if snapshot.is_closed() {
        return Err(AgentError::new("cannot run a closed workflow"));
    }
    if snapshot.unresolved_requirement_ids().is_empty() {
        return Err(AgentError::new("no unresolved requirements to run"));
    }

    let program = env::var_os("DRIFTCTL_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let prompt = continuity_prompt(snapshot);
    let protected_files = protected_files(root)?;

    // OpenAI documents this non-interactive JSONL invocation and recommends
    // the explicit workspace-write sandbox for automation that makes edits.
    // Source: https://developers.openai.com/codex/noninteractive/
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .args([
            "exec",
            "--json",
            "--ephemeral",
            "--sandbox",
            "workspace-write",
        ])
        .arg(prompt);
    let output = capture_process(command)?;
    let protected_changes = changed_protected_files(root, &protected_files)?;

    if output.stdout.exceeded {
        return Err(AgentError::new("Codex trajectory exceeds the size limit"));
    }
    if !protected_changes.is_empty() {
        return Err(AgentError::new(format!(
            "Codex modified protected files: {}",
            protected_changes.join(", ")
        )));
    }
    let terminal = validate_codex_jsonl(&output.stdout.bytes)?;
    let trajectory = persist_trajectory(root, &output.stdout.bytes)?;
    let coherent_exit = matches!(terminal, TerminalEvent::Completed) == output.status.success();
    if !coherent_exit {
        return Err(AgentError::new(format!(
            "Codex terminal event does not match {}; trajectory saved to {}",
            output.status,
            display_path(root, &trajectory)
        )));
    }
    if matches!(terminal, TerminalEvent::Failed) {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        let detail = stderr.trim();
        let truncation = if output.stderr.exceeded {
            " (truncated)"
        } else {
            ""
        };
        return Err(AgentError::new(if detail.is_empty() {
            format!(
                "Codex exited with {}; trajectory saved to {}",
                output.status,
                display_path(root, &trajectory)
            )
        } else {
            format!(
                "Codex exited with {}: {detail}{truncation}; trajectory saved to {}",
                output.status,
                display_path(root, &trajectory)
            )
        }));
    }

    Ok(trajectory)
}

fn capture_process(mut command: Command) -> Result<ProcessOutput, AgentError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AgentError::new(format!("could not launch Codex: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::new("could not capture Codex stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::new("could not capture Codex stderr"))?;

    let stdout_reader = thread::spawn(move || read_bounded(stdout, MAX_TRAJECTORY_BYTES));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, MAX_STDERR_BYTES));
    let status = child
        .wait()
        .map_err(|error| AgentError::new(format!("could not wait for Codex: {error}")))?;
    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded(mut source: impl Read, limit: usize) -> io::Result<CapturedStream> {
    let mut bytes = Vec::new();
    let mut exceeded = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = source.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    Ok(CapturedStream { bytes, exceeded })
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<CapturedStream>>,
    stream: &str,
) -> Result<CapturedStream, AgentError> {
    reader
        .join()
        .map_err(|_| AgentError::new(format!("Codex {stream} reader panicked")))?
        .map_err(|error| AgentError::new(format!("could not read Codex {stream}: {error}")))
}

fn protected_files(root: &Path) -> Result<Vec<ProtectedFile>, AgentError> {
    let state_directory = root.join(".driftctl");
    let state_metadata = fs::symlink_metadata(&state_directory)
        .map_err(|error| AgentError::new(format!("could not inspect .driftctl: {error}")))?;
    if state_metadata.file_type().is_symlink() || !state_metadata.is_dir() {
        return Err(AgentError::new(
            ".driftctl must be a real directory before running an agent",
        ));
    }

    ["AGENTS.md", "CLAUDE.md", ".driftctl/ledger.jsonl"]
        .into_iter()
        .map(|label| ProtectedFile::capture(root, label))
        .collect()
}

impl ProtectedFile {
    fn capture(root: &Path, label: &str) -> Result<Self, AgentError> {
        let path = root.join(label);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
                AgentError::new(format!("protected path must be a regular file: {label}")),
            ),
            Ok(metadata) => Ok(Self {
                label: label.to_owned(),
                bytes: Some(fs::read(&path).map_err(|error| AgentError::new(error.to_string()))?),
                readonly: Some(metadata.permissions().readonly()),
                path,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                label: label.to_owned(),
                path,
                bytes: None,
                readonly: None,
            }),
            Err(error) => Err(AgentError::new(format!(
                "could not inspect protected path {label}: {error}"
            ))),
        }
    }

    fn changed(&self) -> Result<bool, AgentError> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Ok(true),
            Ok(metadata) => {
                let bytes = fs::read(&self.path).map_err(|error| {
                    AgentError::new(format!("could not verify {}: {error}", self.label))
                })?;
                Ok(self.bytes.as_deref() != Some(bytes.as_slice())
                    || self.readonly != Some(metadata.permissions().readonly()))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(self.bytes.is_some()),
            Err(error) => Err(AgentError::new(format!(
                "could not verify protected path {}: {error}",
                self.label
            ))),
        }
    }
}

fn changed_protected_files(
    root: &Path,
    protected_files: &[ProtectedFile],
) -> Result<Vec<String>, AgentError> {
    let state_metadata = fs::symlink_metadata(root.join(".driftctl"))
        .map_err(|error| AgentError::new(format!("could not verify .driftctl: {error}")))?;
    if state_metadata.file_type().is_symlink() || !state_metadata.is_dir() {
        return Ok(vec![".driftctl".to_owned()]);
    }

    protected_files
        .iter()
        .filter_map(|file| match file.changed() {
            Ok(true) => Some(Ok(file.label.clone())),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn continuity_prompt(snapshot: &Snapshot) -> String {
    let mut prompt = format!(
        "Work on the current repository and complete the following durable task.\n\nGoal: {}\n\nUnresolved requirements:\n",
        snapshot.goal()
    );
    for requirement in snapshot
        .requirements()
        .iter()
        .filter(|requirement| requirement.evidence().is_none())
    {
        prompt.push_str(&format!("- {}: {}\n", requirement.id(), requirement.text()));
    }
    prompt.push_str(
        "\nMake the smallest complete change and run the relevant tests. Follow all existing repository instructions. Do not edit .driftctl or any harness instruction/configuration file. Do not claim the workflow is verified: closure requires external evidence after this run.\n",
    );
    prompt
}

fn validate_codex_jsonl(bytes: &[u8]) -> Result<TerminalEvent, AgentError> {
    if bytes.is_empty() {
        return Err(AgentError::new("Codex emitted an empty trajectory"));
    }
    if bytes.len() > MAX_TRAJECTORY_BYTES {
        return Err(AgentError::new("Codex trajectory exceeds the size limit"));
    }
    if !bytes.ends_with(b"\n") {
        return Err(AgentError::new(
            "Codex trajectory does not end with a complete JSONL record",
        ));
    }

    let mut saw_thread = false;
    let mut saw_turn = false;
    let mut terminal = None;
    for (index, line) in bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            return Err(AgentError::new(format!(
                "Codex trajectory contains a blank event at line {}",
                index + 1
            )));
        }
        if index >= MAX_TRAJECTORY_EVENTS {
            return Err(AgentError::new("Codex trajectory has too many events"));
        }
        let event: Value = serde_json::from_slice(line).map_err(|error| {
            AgentError::new(format!("invalid Codex JSONL event {}: {error}", index + 1))
        })?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::new(format!("Codex event {} has no type", index + 1)))?;
        match event_type {
            "thread.started" if !saw_thread && !saw_turn && terminal.is_none() => {
                saw_thread = true;
            }
            "turn.started" if saw_thread && !saw_turn && terminal.is_none() => {
                saw_turn = true;
            }
            "item.started" | "item.updated" | "item.completed" | "error"
                if saw_thread && saw_turn && terminal.is_none() => {}
            "turn.completed" if saw_thread && saw_turn && terminal.is_none() => {
                terminal = Some(TerminalEvent::Completed);
            }
            "turn.failed" if saw_thread && saw_turn && terminal.is_none() => {
                terminal = Some(TerminalEvent::Failed);
            }
            _ => {
                return Err(AgentError::new(format!(
                    "unexpected Codex event order or type at line {}: {event_type}",
                    index + 1
                )));
            }
        }
    }
    if !saw_thread {
        return Err(AgentError::new(
            "Codex trajectory has no thread.started event",
        ));
    }
    terminal.ok_or_else(|| AgentError::new("Codex trajectory has no terminal turn event"))
}

fn persist_trajectory(root: &Path, bytes: &[u8]) -> Result<PathBuf, AgentError> {
    let directory = root.join(".driftctl/trajectories");
    fs::create_dir_all(&directory).map_err(|error| AgentError::new(error.to_string()))?;

    for sequence in 1_u32..=9999 {
        let path = directory.join(format!("run-{sequence:04}.jsonl"));
        let file = OpenOptions::new().write(true).create_new(true).open(&path);
        match file {
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|error| AgentError::new(error.to_string()))?;
                file.sync_data()
                    .map_err(|error| AgentError::new(error.to_string()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(AgentError::new(error.to_string())),
        }
    }
    Err(AgentError::new("trajectory sequence exhausted"))
}

pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
