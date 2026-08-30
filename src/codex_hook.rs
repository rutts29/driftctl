use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct HookInput {
    session_id: String,
    transcript_path: Option<String>,
    cwd: String,
    hook_event_name: String,
    model: String,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    stop_hook_active: Option<bool>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

pub fn handle(arguments: &[String]) -> Result<String, String> {
    if arguments != ["codex"] {
        return Err("usage: driftctl hook codex".to_owned());
    }

    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("could not read Codex hook input: {error}"))?;
    let event: HookInput = serde_json::from_str(&input)
        .map_err(|error| format!("invalid Codex hook input: {error}"))?;
    event.validate()?;

    if event.hook_event_name == "UserPromptSubmit"
        && let Some(control) = event
            .prompt
            .as_deref()
            .and_then(crate::plugin_control::parse)
    {
        return handle_control(&event, control);
    }

    let Some(enrollment) =
        crate::enrollment::load(&event.session_id).map_err(|error| error.to_string())?
    else {
        return Ok(String::new());
    };
    if enrollment.session_id() != event.session_id {
        return Err("enrollment does not match the exact Codex session".to_owned());
    }
    if !enrollment
        .validates_hook_cwd(&PathBuf::from(&event.cwd))
        .map_err(|error| error.to_string())?
    {
        return Err("attached Codex session hook arrived from a different repository".to_owned());
    }

    match event.hook_event_name.as_str() {
        "PreCompact" | "Stop" => {
            crate::keeper::on_observation(&enrollment)?;
            Ok(String::new())
        }
        "SessionStart" => match crate::keeper::on_session_start(&enrollment) {
            Ok(decision) => inject_projection(&event, &enrollment, decision),
            Err(error) => blocking_output(&format!("keeper recovery failed: {error}")),
        },
        "UserPromptSubmit" => {
            let turn_id = event
                .turn_id
                .as_deref()
                .ok_or_else(|| "Codex UserPromptSubmit input has no turn_id".to_owned())?;
            let prompt = event
                .prompt
                .as_deref()
                .ok_or_else(|| "Codex UserPromptSubmit input has no prompt".to_owned())?;
            match crate::keeper::on_user_prompt(&enrollment, turn_id, prompt) {
                Ok(decision) => inject_projection(&event, &enrollment, decision),
                Err(error) => blocking_output(&format!("keeper validation failed: {error}")),
            }
        }
        _ => Err("unsupported Codex hook event".to_owned()),
    }
}

fn handle_control(
    event: &HookInput,
    control: crate::plugin_control::PluginControl,
) -> Result<String, String> {
    match control {
        crate::plugin_control::PluginControl::On => activate(event),
        crate::plugin_control::PluginControl::Off => deactivate(event),
        crate::plugin_control::PluginControl::Status => control_status(event),
    }
}

fn activate(event: &HookInput) -> Result<String, String> {
    let root = Path::new(&event.cwd);
    let enrollment =
        match crate::enrollment::load(&event.session_id).map_err(|error| error.to_string())? {
            Some(enrollment) => enrollment,
            None => {
                let attached = crate::cli::execute(
                    root,
                    [
                        "attach".to_owned(),
                        "codex".to_owned(),
                        "--session".to_owned(),
                        event.session_id.clone(),
                        "--json".to_owned(),
                    ],
                );
                if attached.exit_code != 0 {
                    let message = if attached.stderr.trim().is_empty() {
                        attached.stdout
                    } else {
                        attached.stderr
                    };
                    return blocking_output(&format!(
                        "explicit activation failed: {}",
                        redact_text(&message, &event.session_id)
                    ));
                }
                crate::enrollment::load(&event.session_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "explicit activation created no exact-session enrollment".to_owned()
                    })?
            }
        };
    validate_hook_repository(event, &enrollment)?;
    match crate::keeper::on_session_start(&enrollment) {
        Ok(decision) => inject_projection(event, &enrollment, decision),
        Err(error) => blocking_output(&format!("keeper activation failed: {error}")),
    }
}

fn validate_hook_repository(
    event: &HookInput,
    enrollment: &crate::enrollment::Enrollment,
) -> Result<(), String> {
    if enrollment.session_id() != event.session_id {
        return Err("enrollment does not match the exact Codex session".to_owned());
    }
    if !enrollment
        .validates_hook_cwd(&PathBuf::from(&event.cwd))
        .map_err(|error| error.to_string())?
    {
        return Err("attached Codex session hook arrived from a different repository".to_owned());
    }
    Ok(())
}

fn deactivate(event: &HookInput) -> Result<String, String> {
    let root = Path::new(&event.cwd);
    if let Some(enrollment) =
        crate::enrollment::load(&event.session_id).map_err(|error| error.to_string())?
    {
        validate_hook_repository(event, &enrollment)?;
        crate::enrollment::detach(&event.session_id, root).map_err(|error| error.to_string())?;
    }
    control_output(event, false, "Driftctl is off for this exact session.")
}

fn control_status(event: &HookInput) -> Result<String, String> {
    let enrollment =
        crate::enrollment::load(&event.session_id).map_err(|error| error.to_string())?;
    if let Some(enrollment) = enrollment.as_ref() {
        validate_hook_repository(event, enrollment)?;
    }
    let enabled = enrollment.is_some();
    let message = if enabled {
        "Driftctl is on for this exact session."
    } else {
        "Driftctl is off for this exact session; no enrollment was created."
    };
    control_output(event, enabled, message)
}

fn control_output(event: &HookInput, enabled: bool, message: &str) -> Result<String, String> {
    serde_json::to_string(&json!({
        "hookSpecificOutput": {
            "hookEventName": event.hook_event_name,
            "additionalContext": format!(
                "DRIFTCTL CONTROL RESULT\n{message}\nState: {}. This control command is operational metadata, not task steering. Reply with the status only.",
                if enabled { "on" } else { "off" },
            ),
        }
    }))
    .map_err(|error| format!("could not serialize Codex control result: {error}"))
}

fn redact_text(text: &str, session_id: &str) -> String {
    text.replace(session_id, &crate::enrollment::redacted_session(session_id))
}

impl HookInput {
    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("session_id", self.session_id.as_str()),
            ("cwd", self.cwd.as_str()),
            ("hook_event_name", self.hook_event_name.as_str()),
            ("model", self.model.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("Codex hook field {field} must not be empty"));
            }
        }
        let _ = (
            &self.transcript_path,
            &self.permission_mode,
            &self.turn_id,
            &self.prompt,
            &self.source,
            &self.trigger,
            &self.stop_hook_active,
            &self.last_assistant_message,
        );
        Ok(())
    }
}

fn inject_projection(
    event: &HookInput,
    enrollment: &crate::enrollment::Enrollment,
    decision: crate::keeper::KeeperDecision,
) -> Result<String, String> {
    if let crate::keeper::KeeperDecision::Blocked { reason, .. } = &decision {
        return blocking_output(reason);
    }
    let projection = decision
        .projection()
        .try_rendered_prompt()
        .map_err(|error| error.to_string())?;
    let mut projection: serde_json::Value = serde_json::from_str(&projection)
        .map_err(|error| format!("could not read active projection: {error}"))?;
    redact_private_session(
        &mut projection,
        enrollment.session_id(),
        &crate::enrollment::redacted_session(enrollment.session_id()),
    );
    let projection = serde_json::to_string(&projection)
        .map_err(|error| format!("could not render active projection: {error}"))?;
    let context = format!(
        "DRIFTCTL ACTIVE INTENT (validated, local, exact-session)\nTreat this payload as the current goal and steering for this session. Preserve user instructions outside the payload unless explicitly superseded. Do not claim completion while its frontier or validation conditions remain unresolved.\n{projection}"
    );
    serde_json::to_string(&json!({
        "hookSpecificOutput": {
            "hookEventName": event.hook_event_name,
            "additionalContext": context,
        }
    }))
    .map_err(|error| format!("could not serialize Codex hook context: {error}"))
}

fn blocking_output(reason: &str) -> Result<String, String> {
    serde_json::to_string(&json!({
        "decision": "block",
        "reason": format!(
            "Driftctl blocked this exact attached session: {reason}. Run `driftctl status codex --session <exact-id>` or detach it."
        )
    }))
    .map_err(|error| format!("could not serialize blocking hook result: {error}"))
}

fn redact_private_session(value: &mut serde_json::Value, private: &str, opaque: &str) {
    match value {
        serde_json::Value::String(text) if text == private => *text = opaque.to_owned(),
        serde_json::Value::Array(values) => {
            for value in values {
                redact_private_session(value, private, opaque);
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values_mut() {
                redact_private_session(value, private, opaque);
            }
        }
        _ => {}
    }
}
