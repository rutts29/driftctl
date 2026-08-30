//! Digest-only receipts for user prompts accepted through lifecycle hooks.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::run_store::RunStore;

const SCHEMA_VERSION: u32 = 1;
const DIRECTORY: &str = "observations";
const FILE: &str = "hook-prompts.jsonl";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct HookPromptReceipt {
    schema_version: u32,
    record_id: String,
    content_digest: String,
    projection_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_record_id: Option<String>,
}

pub(crate) struct HookEventStore {
    directory: PathBuf,
}

impl HookEventStore {
    pub(crate) fn prompt_digest(content: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(b"driftctl-hook-prompt-v1\0user\0");
        digest.update(content.as_bytes());
        format!("sha256:{:x}", digest.finalize())
    }

    pub(crate) fn open(run: &RunStore) -> Result<Self, HookEventStoreError> {
        let directory = run.path().join(DIRECTORY);
        create_private_directory(&directory)?;
        Ok(Self { directory })
    }

    pub(crate) fn matches(
        &self,
        record_id: &str,
        content_digest: &str,
    ) -> Result<bool, HookEventStoreError> {
        Ok(self
            .load()?
            .get(record_id)
            .is_some_and(|receipt| receipt.content_digest == content_digest))
    }

    pub(crate) fn conflicts(
        &self,
        record_id: &str,
        content_digest: &str,
    ) -> Result<bool, HookEventStoreError> {
        Ok(self
            .load()?
            .get(record_id)
            .is_some_and(|receipt| receipt.content_digest != content_digest))
    }

    pub(crate) fn record(
        &self,
        record_id: &str,
        content_digest: &str,
        projection_revision: u64,
    ) -> Result<(), HookEventStoreError> {
        let existing = self.load()?;
        if let Some(receipt) = existing.get(record_id) {
            return if receipt.content_digest == content_digest {
                Ok(())
            } else {
                Err(HookEventStoreError::Invalid(
                    "hook turn ID was already accepted with different content",
                ))
            };
        }
        let receipt = HookPromptReceipt {
            schema_version: SCHEMA_VERSION,
            record_id: record_id.to_owned(),
            content_digest: content_digest.to_owned(),
            projection_revision,
            provider_record_id: None,
        };
        validate(&receipt)?;
        self.append(&receipt)
    }

    /// Bind one persisted provider message to exactly one previously accepted
    /// hook prompt. Codex does not preserve the hook turn ID as the persisted
    /// user-message ID, so the first binding uses the content digest inside the
    /// already enrolled exact session. The durable provider ID prevents a
    /// later identical prompt from reusing the same receipt.
    pub(crate) fn claim_provider_record(
        &self,
        provider_record_id: &str,
        content_digest: &str,
    ) -> Result<bool, HookEventStoreError> {
        if provider_record_id.trim().is_empty()
            || provider_record_id.chars().any(char::is_control)
            || !content_digest.starts_with("sha256:")
        {
            return Err(HookEventStoreError::Invalid(
                "invalid persisted provider prompt",
            ));
        }
        let receipts = self.load()?;
        if let Some(receipt) = receipts
            .values()
            .find(|receipt| receipt.provider_record_id.as_deref() == Some(provider_record_id))
        {
            return if receipt.content_digest == content_digest {
                Ok(true)
            } else {
                Err(HookEventStoreError::Invalid(
                    "persisted provider prompt changed after hook acceptance",
                ))
            };
        }
        if let Some(receipt) = receipts.get(provider_record_id)
            && receipt.content_digest != content_digest
        {
            return Err(HookEventStoreError::Invalid(
                "provider prompt ID conflicts with an accepted hook receipt",
            ));
        }
        let candidate = receipts
            .values()
            .find(|receipt| {
                receipt.provider_record_id.is_none()
                    && receipt.content_digest == content_digest
                    && (receipt.record_id == provider_record_id
                        || !receipts.contains_key(provider_record_id))
            })
            .cloned();
        let Some(mut candidate) = candidate else {
            return Ok(false);
        };
        candidate.provider_record_id = Some(provider_record_id.to_owned());
        validate(&candidate)?;
        self.append(&candidate)?;
        Ok(true)
    }

    fn append(&self, receipt: &HookPromptReceipt) -> Result<(), HookEventStoreError> {
        let path = self.directory.join(FILE);
        ensure_private_regular_or_missing(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| io_error("open hook receipt store", &path, error))?;
        serde_json::to_writer(&mut file, receipt)
            .map_err(|error| HookEventStoreError::Serialization(error.to_string()))?;
        file.write_all(b"\n")
            .map_err(|error| io_error("write hook receipt", &path, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync hook receipt", &path, error))
    }

    fn load(&self) -> Result<BTreeMap<String, HookPromptReceipt>, HookEventStoreError> {
        let path = self.directory.join(FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(io_error("inspect hook receipt store", &path, error)),
        };
        ensure_private_regular(&path, &metadata)?;
        let input = fs::read_to_string(&path)
            .map_err(|error| io_error("read hook receipt store", &path, error))?;
        let mut receipts = BTreeMap::new();
        for line in input.lines() {
            let receipt: HookPromptReceipt = serde_json::from_str(line)
                .map_err(|error| HookEventStoreError::Serialization(error.to_string()))?;
            validate(&receipt)?;
            match receipts.get(&receipt.record_id) {
                None => {
                    receipts.insert(receipt.record_id.clone(), receipt);
                }
                Some(existing) if existing == &receipt => {}
                Some(existing)
                    if existing.provider_record_id.is_none()
                        && receipt.provider_record_id.is_some()
                        && existing.schema_version == receipt.schema_version
                        && existing.record_id == receipt.record_id
                        && existing.content_digest == receipt.content_digest
                        && existing.projection_revision == receipt.projection_revision =>
                {
                    receipts.insert(receipt.record_id.clone(), receipt);
                }
                Some(_) => {
                    return Err(HookEventStoreError::Invalid(
                        "hook receipt store contains conflicting duplicate IDs",
                    ));
                }
            }
        }
        Ok(receipts)
    }
}

#[derive(Debug)]
pub(crate) enum HookEventStoreError {
    Invalid(&'static str),
    Io {
        action: &'static str,
        path: PathBuf,
        message: String,
    },
    Serialization(String),
}

impl fmt::Display for HookEventStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Serialization(message) => formatter.write_str(message),
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

impl std::error::Error for HookEventStoreError {}

fn validate(receipt: &HookPromptReceipt) -> Result<(), HookEventStoreError> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.record_id.trim().is_empty()
        || receipt.record_id.chars().any(char::is_control)
        || !receipt.content_digest.starts_with("sha256:")
        || receipt.projection_revision == 0
        || receipt
            .provider_record_id
            .as_ref()
            .is_some_and(|record_id| {
                record_id.trim().is_empty() || record_id.chars().any(char::is_control)
            })
    {
        Err(HookEventStoreError::Invalid("invalid hook prompt receipt"))
    } else {
        Ok(())
    }
}

fn create_private_directory(path: &Path) -> Result<(), HookEventStoreError> {
    fs::create_dir_all(path)
        .map_err(|error| io_error("create hook receipt directory", path, error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect hook receipt directory", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(HookEventStoreError::Invalid(
            "hook receipt path must be a private directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("protect hook receipt directory", path, error))?;
    }
    Ok(())
}

fn ensure_private_regular_or_missing(path: &Path) -> Result<(), HookEventStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_private_regular(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect hook receipt file", path, error)),
    }
}

fn ensure_private_regular(
    _path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), HookEventStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HookEventStoreError::Invalid(
            "hook receipt state must be a private regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(HookEventStoreError::Invalid(
                "hook receipt state is accessible by another user",
            ));
        }
    }
    Ok(())
}

fn io_error(action: &'static str, path: impl AsRef<Path>, error: io::Error) -> HookEventStoreError {
    HookEventStoreError::Io {
        action,
        path: path.as_ref().to_owned(),
        message: error.to_string(),
    }
}
