//! Strict, provider-neutral intake for local session transcripts.
//!
//! This module intentionally only preserves normalized source records.  It
//! does not infer intent, build a projection, or grant authority beyond
//! identifying explicit user records.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::intent_history::{SourceProvider, SourceRef, SourceRole};

/// The only neutral-session-bundle schema this release accepts.
pub const SESSION_BUNDLE_SCHEMA_VERSION: u32 = 1;

const MAX_IDENTIFIER_BYTES: usize = 16 * 1024;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;

/// The private provenance shared by every record in a bundle.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct BundleSource {
    provider: SourceProvider,
    session_ref: String,
    repository_digest: String,
    head: String,
    digest: String,
}

impl BundleSource {
    #[must_use]
    pub fn provider(&self) -> SourceProvider {
        self.provider.clone()
    }

    /// Local-only source locator. Do not include this in a sanitized export.
    #[must_use]
    pub fn session_ref_private(&self) -> &str {
        &self.session_ref
    }

    #[must_use]
    pub fn repository_digest(&self) -> &str {
        &self.repository_digest
    }

    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl fmt::Debug for BundleSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BundleSource")
            .field("provider", &self.provider)
            .field("repository_digest", &self.repository_digest)
            .field("head", &self.head)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

/// One normalized source message, retained in the provider's original order.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct BundleRecord {
    id: String,
    role: SourceRole,
    content: String,
    content_digest: String,
}

impl BundleRecord {
    /// Creates a record and calculates its SHA-256 digest from its normalized
    /// identifier, role, and text.
    pub fn new(
        id: impl Into<String>,
        role: SourceRole,
        content: impl Into<String>,
    ) -> Result<Self, SessionBundleError> {
        let id = id.into();
        let content = normalize_content(content.into());
        validate_identifier(&id, "record id")?;
        validate_content(&content)?;
        let content_digest = record_digest(&id, &role, &content);
        Ok(Self {
            id,
            role,
            content,
            content_digest,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn role(&self) -> SourceRole {
        self.role.clone()
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }

    #[must_use]
    pub fn is_authoritative(&self) -> bool {
        matches!(self.role, SourceRole::User)
    }
}

impl fmt::Debug for BundleRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BundleRecord")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("content_digest", &self.content_digest)
            .finish()
    }
}

/// A strict, versioned session intake document.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct NeutralSessionBundle {
    schema_version: u32,
    source: BundleSource,
    records: Vec<BundleRecord>,
}

impl NeutralSessionBundle {
    /// Builds a bundle from provider records, deriving source head and digest.
    pub fn from_records(
        provider: SourceProvider,
        session_ref: impl Into<String>,
        repository_digest: impl Into<String>,
        records: Vec<BundleRecord>,
    ) -> Result<Self, SessionBundleError> {
        validate_records(&records)?;
        let session_ref = session_ref.into();
        let repository_digest = repository_digest.into();
        validate_identifier(&session_ref, "source session_ref")?;
        validate_sha256(&repository_digest, "source repository_digest")?;
        let source = BundleSource {
            provider,
            session_ref,
            repository_digest,
            head: records
                .last()
                .expect("validated non-empty records")
                .id
                .clone(),
            digest: source_digest(&records),
        };
        Ok(Self {
            schema_version: SESSION_BUNDLE_SCHEMA_VERSION,
            source,
            records,
        })
    }

    /// Parses an exact schema-v1 JSON document and validates every field.
    pub fn from_json(json: &str) -> Result<Self, SessionBundleError> {
        let wire: WireBundle = serde_json::from_str(json)
            .map_err(|error| SessionBundleError::Json(error.to_string()))?;
        Self::try_from(wire)
    }

    /// Emits the strict schema-v1 document for local handoff.
    pub fn to_json(&self) -> Result<String, SessionBundleError> {
        serde_json::to_string(self).map_err(|error| SessionBundleError::Json(error.to_string()))
    }

    #[must_use]
    pub fn source(&self) -> &BundleSource {
        &self.source
    }

    #[must_use]
    pub fn records(&self) -> &[BundleRecord] {
        &self.records
    }

    /// Only explicit user text may be used as semantic authority by a later
    /// resolver. Other roles remain available as source evidence/context.
    #[must_use]
    pub fn authoritative_records(&self) -> Vec<&BundleRecord> {
        self.records
            .iter()
            .filter(|record| record.is_authoritative())
            .collect()
    }

    /// Converts preserved records to the typed provenance references used by
    /// later history layers, without making any semantic decision.
    #[must_use]
    pub fn source_refs(&self) -> Vec<SourceRef> {
        self.records
            .iter()
            .map(|record| {
                SourceRef::new(
                    self.source.provider.clone(),
                    self.source.session_ref.clone(),
                    record.id.clone(),
                    record.role.clone(),
                    record.content_digest.clone(),
                )
            })
            .collect()
    }

    /// Enforces the additional precondition a projection resolver will need.
    pub fn validate_for_projection(&self) -> Result<(), SessionBundleError> {
        if self.authoritative_records().is_empty() {
            return Err(SessionBundleError::NoAuthoritativeUserRecord);
        }
        Ok(())
    }

    #[must_use]
    pub fn sanitized_summary(&self) -> SanitizedBundleSummary {
        SanitizedBundleSummary {
            schema_version: self.schema_version,
            provider: self.source.provider.clone(),
            repository_digest: self.source.repository_digest.clone(),
            source_head: self.source.head.clone(),
            source_digest: self.source.digest.clone(),
            record_count: self.records.len(),
            authoritative_user_record_count: self.authoritative_records().len(),
        }
    }
}

impl fmt::Debug for NeutralSessionBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NeutralSessionBundle")
            .field("schema_version", &self.schema_version)
            .field("source", &self.source)
            .field("record_count", &self.records.len())
            .field(
                "authoritative_user_record_count",
                &self.authoritative_records().len(),
            )
            .finish()
    }
}

/// Rendering-safe metadata for logs and public inspection output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SanitizedBundleSummary {
    schema_version: u32,
    provider: SourceProvider,
    repository_digest: String,
    source_head: String,
    source_digest: String,
    record_count: usize,
    authoritative_user_record_count: usize,
}

impl fmt::Display for SanitizedBundleSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "schema_version: {}\nprovider: {:?}\nrepository_digest: {}\nsource_head: {}\nsource_digest: {}\nrecords: {}\nauthoritative_user_records: {}",
            self.schema_version,
            self.provider,
            self.repository_digest,
            self.source_head,
            self.source_digest,
            self.record_count,
            self.authoritative_user_record_count,
        )
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum SessionBundleError {
    Json(String),
    UnsupportedSchema(u32),
    InvalidField(&'static str),
    InvalidDigest(&'static str),
    DuplicateRecordId(String),
    EmptyRecords,
    NoAuthoritativeUserRecord,
    SourceHeadMismatch,
    SourceDigestMismatch,
}

impl fmt::Display for SessionBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid neutral session bundle JSON: {error}"),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported neutral session bundle schema version: {version}"
                )
            }
            Self::InvalidField(field) => {
                write!(formatter, "invalid neutral session bundle field: {field}")
            }
            Self::InvalidDigest(field) => {
                write!(formatter, "invalid SHA-256 digest in field: {field}")
            }
            Self::DuplicateRecordId(id) => write!(
                formatter,
                "duplicate neutral session bundle record ID: {id}"
            ),
            Self::EmptyRecords => formatter.write_str("neutral session bundle has no records"),
            Self::NoAuthoritativeUserRecord => {
                formatter.write_str("neutral session bundle has no explicit user text")
            }
            Self::SourceHeadMismatch => {
                formatter.write_str("neutral session bundle source head does not match records")
            }
            Self::SourceDigestMismatch => {
                formatter.write_str("neutral session bundle source digest does not match records")
            }
        }
    }
}

impl std::error::Error for SessionBundleError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBundle {
    schema_version: u32,
    source: WireSource,
    records: Vec<WireRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSource {
    provider: SourceProvider,
    session_ref: String,
    repository_digest: String,
    head: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    id: String,
    role: SourceRole,
    content: String,
    content_digest: String,
}

impl<'de> Deserialize<'de> for NeutralSessionBundle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WireBundle::deserialize(deserializer)?;
        NeutralSessionBundle::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<WireBundle> for NeutralSessionBundle {
    type Error = SessionBundleError;

    fn try_from(wire: WireBundle) -> Result<Self, Self::Error> {
        if wire.schema_version != SESSION_BUNDLE_SCHEMA_VERSION {
            return Err(SessionBundleError::UnsupportedSchema(wire.schema_version));
        }
        validate_identifier(&wire.source.session_ref, "source session_ref")?;
        validate_sha256(&wire.source.repository_digest, "source repository_digest")?;
        validate_identifier(&wire.source.head, "source head")?;
        validate_sha256(&wire.source.digest, "source digest")?;

        let records = wire
            .records
            .into_iter()
            .map(|record| {
                validate_identifier(&record.id, "record id")?;
                validate_content(&record.content)?;
                if record.content != normalize_content(record.content.clone()) {
                    return Err(SessionBundleError::InvalidField(
                        "record content must be normalized",
                    ));
                }
                validate_sha256(&record.content_digest, "record content_digest")?;
                if record.content_digest != record_digest(&record.id, &record.role, &record.content)
                {
                    return Err(SessionBundleError::InvalidDigest("record content_digest"));
                }
                Ok(BundleRecord {
                    id: record.id,
                    role: record.role,
                    content: record.content,
                    content_digest: record.content_digest,
                })
            })
            .collect::<Result<Vec<_>, SessionBundleError>>()?;
        validate_records(&records)?;
        if wire.source.head != records.last().expect("validated non-empty records").id {
            return Err(SessionBundleError::SourceHeadMismatch);
        }
        if wire.source.digest != source_digest(&records) {
            return Err(SessionBundleError::SourceDigestMismatch);
        }

        Ok(Self {
            schema_version: wire.schema_version,
            source: BundleSource {
                provider: wire.source.provider,
                session_ref: wire.source.session_ref,
                repository_digest: wire.source.repository_digest,
                head: wire.source.head,
                digest: wire.source.digest,
            },
            records,
        })
    }
}

fn validate_records(records: &[BundleRecord]) -> Result<(), SessionBundleError> {
    if records.is_empty() {
        return Err(SessionBundleError::EmptyRecords);
    }
    let mut ids = BTreeSet::new();
    for record in records {
        if !ids.insert(&record.id) {
            return Err(SessionBundleError::DuplicateRecordId(record.id.clone()));
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), SessionBundleError> {
    if value.trim().is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(SessionBundleError::InvalidField(field));
    }
    Ok(())
}

fn validate_content(value: &str) -> Result<(), SessionBundleError> {
    if value.is_empty() || value.len() > MAX_CONTENT_BYTES || value.contains('\0') {
        return Err(SessionBundleError::InvalidField("record content"));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), SessionBundleError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(SessionBundleError::InvalidDigest(field));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SessionBundleError::InvalidDigest(field));
    }
    Ok(())
}

fn normalize_content(content: String) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn record_digest(id: &str, role: &SourceRole, content: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(id.as_bytes());
    digest.update([0]);
    digest.update(role_name(role).as_bytes());
    digest.update([0]);
    digest.update(content.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn source_digest(records: &[BundleRecord]) -> String {
    let mut digest = Sha256::new();
    for record in records {
        digest.update(record.id.as_bytes());
        digest.update([0]);
        digest.update(record.content_digest.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn role_name(role: &SourceRole) -> &'static str {
    match role {
        SourceRole::User => "user",
        SourceRole::Assistant => "assistant",
        SourceRole::Tool => "tool",
        SourceRole::SystemObservation => "system_observation",
    }
}
