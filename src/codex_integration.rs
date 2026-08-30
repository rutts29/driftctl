//! Self-contained installation of Driftctl's isolated Codex plugin.

use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

const MARKETPLACE_NAME: &str = "driftctl-local";
const PLUGIN_ID: &str = "driftctl-codex@driftctl-local";
const MARKETPLACE: &str = include_str!("../.agents/plugins/marketplace.json");
const PLUGIN_MANIFEST: &str = include_str!("../plugins/driftctl-codex/.codex-plugin/plugin.json");
const HOOKS: &str = include_str!("../plugins/driftctl-codex/hooks/hooks.json");
const CONTROL_SKILL: &str = include_str!("../plugins/driftctl-codex/skills/driftctl/SKILL.md");
const CONTROL_SKILL_POLICY: &str =
    include_str!("../plugins/driftctl-codex/skills/driftctl/agents/openai.yaml");

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IntegrationAction {
    Install,
    Status,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IntegrationRequest {
    pub action: IntegrationAction,
    pub json: bool,
}

#[derive(Debug)]
pub(crate) enum IntegrationError {
    Invalid(String),
    Io {
        action: &'static str,
        path: PathBuf,
        message: String,
    },
    Codex(String),
    Serialization(String),
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Codex(message) | Self::Serialization(message) => {
                formatter.write_str(message)
            }
            Self::Io {
                action,
                path,
                message,
            } => write!(
                formatter,
                "could not {action} {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for IntegrationError {}

pub(crate) fn parse(arguments: &[String]) -> Result<IntegrationRequest, IntegrationError> {
    let (provider, action, rest) = match arguments {
        [provider, action, rest @ ..] => (provider.as_str(), action.as_str(), rest),
        _ => {
            return Err(IntegrationError::Invalid(
                "usage: driftctl integrate codex install|status|remove [--json]".to_owned(),
            ));
        }
    };
    if provider != "codex" {
        return Err(IntegrationError::Invalid(
            "integrate currently supports exactly: driftctl integrate codex".to_owned(),
        ));
    }
    let action = match action {
        "install" => IntegrationAction::Install,
        "status" => IntegrationAction::Status,
        "remove" => IntegrationAction::Remove,
        _ => {
            return Err(IntegrationError::Invalid(
                "integration action must be install, status, or remove".to_owned(),
            ));
        }
    };
    let json = match rest {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            return Err(IntegrationError::Invalid(
                "integration actions accept only --json".to_owned(),
            ));
        }
    };
    Ok(IntegrationRequest { action, json })
}

pub(crate) fn execute(request: &IntegrationRequest) -> Result<String, IntegrationError> {
    match request.action {
        IntegrationAction::Install => install(request.json),
        IntegrationAction::Status => status(request.json),
        IntegrationAction::Remove => remove(request.json),
    }
}

fn install(json_output: bool) -> Result<String, IntegrationError> {
    require_driftctl_on_path()?;
    require_hooks_feature()?;
    let root = integration_root()?;
    materialize_bundle(&root)?;
    run_codex(&["plugin", "marketplace", "add", path_text(&root)?, "--json"])?;
    run_codex(&["plugin", "add", PLUGIN_ID, "--json"])?;
    let state = observe()?;
    if !state.plugin_installed || !state.hooks_enabled {
        return Err(IntegrationError::Codex(
            "Codex did not report the Driftctl plugin and stable hooks as enabled".to_owned(),
        ));
    }
    render(
        json_output,
        json!({
            "schema_version": 1,
            "provider": "codex",
            "status": "installed",
            "plugin": "installed",
            "hooks_feature": "enabled",
            "hook_trust": "approve Driftctl hooks with `/hooks` on first use",
            "service": "none",
            "data_location": "local",
        }),
        "installed Driftctl's local Codex plugin\nhooks: enabled\ntrust: approve Driftctl hooks with `/hooks` on first use",
    )
}

fn status(json_output: bool) -> Result<String, IntegrationError> {
    let state = observe()?;
    let plugin = if state.plugin_installed {
        "installed"
    } else {
        "not_installed"
    };
    let hooks = if state.hooks_enabled {
        "enabled"
    } else {
        "disabled"
    };
    render(
        json_output,
        json!({
            "schema_version": 1,
            "provider": "codex",
            "status": if state.plugin_installed && state.hooks_enabled {"ready"} else {"blocked"},
            "plugin": plugin,
            "hooks_feature": hooks,
            "hook_trust": "approve Driftctl hooks with `/hooks` on first use",
            "action": if state.plugin_installed {Value::Null} else {json!("run `driftctl integrate codex install`")} ,
        }),
        &format!(
            "plugin: {plugin}\nhooks: {hooks}\ntrust: approve Driftctl hooks with `/hooks` on first use"
        ),
    )
}

fn remove(json_output: bool) -> Result<String, IntegrationError> {
    let root = integration_root()?;
    let state = observe()?;
    if state.plugin_installed {
        run_codex(&["plugin", "remove", PLUGIN_ID, "--json"])?;
    }
    if let Some(observed_root) = state.marketplace_root {
        if observed_root != root {
            return Err(IntegrationError::Codex(
                "refusing to remove a Driftctl-named marketplace owned by another path".to_owned(),
            ));
        }
        run_codex(&[
            "plugin",
            "marketplace",
            "remove",
            MARKETPLACE_NAME,
            "--json",
        ])?;
    }
    remove_owned_bundle(&root)?;
    render(
        json_output,
        json!({
            "schema_version": 1,
            "provider": "codex",
            "status": "removed",
            "plugin": "not_installed",
        }),
        "removed Driftctl's Codex plugin; other Codex configuration was preserved",
    )
}

struct ObservedIntegration {
    plugin_installed: bool,
    hooks_enabled: bool,
    marketplace_root: Option<PathBuf>,
}

fn observe() -> Result<ObservedIntegration, IntegrationError> {
    let plugins = parse_json_output(run_codex(&["plugin", "list", "--json"])?)?;
    let plugin_installed = plugins["installed"].as_array().is_some_and(|plugins| {
        plugins.iter().any(|plugin| {
            plugin["pluginId"] == PLUGIN_ID
                && plugin["installed"] == true
                && plugin["enabled"] == true
        })
    });
    let marketplaces = parse_json_output(run_codex(&["plugin", "marketplace", "list", "--json"])?)?;
    let marketplace_root = marketplaces["marketplaces"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["name"] == MARKETPLACE_NAME)
        })
        .and_then(|entry| entry["root"].as_str())
        .map(PathBuf::from);
    let features = run_codex(&["features", "list"])?;
    let hooks_enabled = String::from_utf8_lossy(&features.stdout)
        .lines()
        .any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"hooks") && fields.last() == Some(&"true")
        });
    Ok(ObservedIntegration {
        plugin_installed,
        hooks_enabled,
        marketplace_root,
    })
}

fn require_hooks_feature() -> Result<(), IntegrationError> {
    let output = run_codex(&["features", "list"])?;
    let enabled = String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields.first() == Some(&"hooks") && fields.last() == Some(&"true")
    });
    if enabled {
        Ok(())
    } else {
        Err(IntegrationError::Codex(
            "Codex lifecycle hooks are unavailable or disabled; update Codex and enable the stable `hooks` feature"
                .to_owned(),
        ))
    }
}

fn require_driftctl_on_path() -> Result<(), IntegrationError> {
    let Some(path) = env::var_os("PATH") else {
        return Err(IntegrationError::Invalid(
            "PATH is unset; install `driftctl` on PATH before integrating Codex".to_owned(),
        ));
    };
    let available = env::split_paths(&path)
        .map(|directory| directory.join(executable_name("driftctl")))
        .any(|candidate| candidate.is_file());
    if available {
        Ok(())
    } else {
        Err(IntegrationError::Invalid(
            "`driftctl` is not on PATH; install this binary on PATH before integrating Codex"
                .to_owned(),
        ))
    }
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn run_codex(arguments: &[&str]) -> Result<Output, IntegrationError> {
    let program = env::var_os("DRIFTCTL_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| {
            IntegrationError::Codex(format!(
                "could not run Codex CLI; install a current `codex` binary: {error}"
            ))
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(IntegrationError::Codex(if message.is_empty() {
            "Codex plugin command failed without an error message".to_owned()
        } else {
            format!("Codex plugin command failed: {message}")
        }))
    }
}

fn parse_json_output(output: Output) -> Result<Value, IntegrationError> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| IntegrationError::Serialization(format!("invalid Codex JSON: {error}")))
}

fn integration_root() -> Result<PathBuf, IntegrationError> {
    let base = match env::var_os("XDG_DATA_HOME") {
        Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
        _ => {
            let home = env::var_os("HOME").ok_or_else(|| {
                IntegrationError::Invalid(
                    "HOME is unset and XDG_DATA_HOME is unavailable".to_owned(),
                )
            })?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(base.join("driftctl/codex-marketplace"))
}

fn materialize_bundle(root: &Path) -> Result<(), IntegrationError> {
    create_private_directory(root)?;
    write_owned_file(
        &root.join(".agents/plugins/marketplace.json"),
        MARKETPLACE.as_bytes(),
    )?;
    write_owned_file(
        &root.join("plugins/driftctl-codex/.codex-plugin/plugin.json"),
        PLUGIN_MANIFEST.as_bytes(),
    )?;
    write_owned_file(
        &root.join("plugins/driftctl-codex/hooks/hooks.json"),
        HOOKS.as_bytes(),
    )?;
    write_owned_file(
        &root.join("plugins/driftctl-codex/skills/driftctl/SKILL.md"),
        CONTROL_SKILL.as_bytes(),
    )?;
    write_owned_file(
        &root.join("plugins/driftctl-codex/skills/driftctl/agents/openai.yaml"),
        CONTROL_SKILL_POLICY.as_bytes(),
    )
}

fn write_owned_file(path: &Path, bytes: &[u8]) -> Result<(), IntegrationError> {
    let parent = path.parent().ok_or_else(|| {
        IntegrationError::Invalid("embedded integration path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(IntegrationError::Invalid(format!(
            "refusing unsafe integration file {}",
            path.display()
        )));
    }
    let temporary = parent.join(format!(".driftctl.tmp-{}", std::process::id()));
    if temporary.exists() {
        return Err(IntegrationError::Invalid(
            "stale Driftctl integration write is present; remove it and retry".to_owned(),
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| io_error("create integration file", &temporary, error))?;
    file.write_all(bytes)
        .map_err(|error| io_error("write integration file", &temporary, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync integration file", &temporary, error))?;
    fs::rename(&temporary, path)
        .map_err(|error| io_error("replace integration file", path, error))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync integration directory", parent, error))
}

fn create_private_directory(path: &Path) -> Result<(), IntegrationError> {
    fs::create_dir_all(path)
        .map_err(|error| io_error("create integration directory", path, error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect integration directory", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(IntegrationError::Invalid(format!(
            "integration path {} is not a safe directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("set integration permissions", path, error))?;
    }
    Ok(())
}

fn remove_owned_bundle(root: &Path) -> Result<(), IntegrationError> {
    if !root.exists() {
        return Ok(());
    }
    let marketplace = root.join(".agents/plugins/marketplace.json");
    let observed = fs::read_to_string(&marketplace)
        .map_err(|error| io_error("verify owned marketplace", &marketplace, error))?;
    if observed != MARKETPLACE {
        return Err(IntegrationError::Invalid(
            "refusing to remove modified integration files; remove them manually after inspection"
                .to_owned(),
        ));
    }
    fs::remove_dir_all(root)
        .map_err(|error| io_error("remove owned integration bundle", root, error))
}

fn path_text(path: &Path) -> Result<&str, IntegrationError> {
    path.to_str()
        .ok_or_else(|| IntegrationError::Invalid("integration path must be valid UTF-8".to_owned()))
}

fn render(json_output: bool, document: Value, human: &str) -> Result<String, IntegrationError> {
    if json_output {
        serde_json::to_string(&document).map_err(|error| {
            IntegrationError::Serialization(format!(
                "could not serialize integration result: {error}"
            ))
        })
    } else {
        Ok(human.to_owned())
    }
}

fn io_error(action: &'static str, path: impl AsRef<Path>, error: io::Error) -> IntegrationError {
    IntegrationError::Io {
        action,
        path: path.as_ref().to_owned(),
        message: error.to_string(),
    }
}
