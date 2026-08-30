use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::agent::{display_path, run_codex};
use crate::codex_source::{self, SessionSelection};
use crate::{ClosureError, Ledger, Snapshot};

const USAGE: &str = "driftctl — durable continuity for coding-agent tasks\n\n\
Usage:\n\
  driftctl start --goal <text> --requirement <text> [--requirement <text> ...]\n\
  driftctl steer --requirement <text>\n\
  driftctl satisfy --id <requirement-id> --evidence <text>\n\
  driftctl status [--json]\n\
  driftctl resume [--json]\n\
  driftctl inspect codex (--last | --session <id>) [--json]\n\
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
    let Ok((selection, json)) = parsed else {
        return CliOutput::error(parsed.expect_err("checked error"));
    };
    match codex_source::inspect(root, selection) {
        Ok(imported) => {
            let output = if json {
                serde_json::to_string(&json!({
                    "schema_version": 1,
                    "provider": "codex",
                    "session": imported.redacted_session(),
                    "imported_user_records": imported.imported_user_record_count(),
                    "source_digest": imported.source_digest(),
                    "blocker": "projection_not_built",
                }))
                .unwrap_or_else(|_| "{\"schema_version\":1}".to_owned())
            } else {
                format!(
                    "provider: codex\nsession: {}\nimported user records: {}\nsource digest: {}\nblocker: projection_not_built",
                    imported.redacted_session(),
                    imported.imported_user_record_count(),
                    imported.source_digest(),
                )
            };
            CliOutput {
                exit_code: 2,
                stdout: output,
                stderr: String::new(),
            }
        }
        Err(error) => CliOutput::error(error.to_string()),
    }
}

fn parse_inspect_arguments(arguments: &[String]) -> Result<(SessionSelection<'_>, bool), String> {
    let Some(provider) = arguments.first() else {
        return Err("inspect requires provider `codex`".to_owned());
    };
    if provider != "codex" {
        return Err("inspect currently supports exactly: driftctl inspect codex".to_owned());
    }
    let mut selection = None;
    let mut json = false;
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
            "--last" | "--session" => {
                return Err("inspect requires exactly one of --last or --session <id>".to_owned());
            }
            "--json" => return Err("--json may only be supplied once".to_owned()),
            option => return Err(format!("unknown inspect option: {option}")),
        }
    }
    selection
        .map(|selection| (selection, json))
        .ok_or_else(|| "inspect requires exactly one of --last or --session <id>".to_owned())
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
