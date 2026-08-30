//! Small coordinator boundary between session inspection and durable run state.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::intent_history::{Event, History, SourceProvider, SourceRef};
use crate::run_store::{
    RecoveredRun, RunId, RunStore, RunStoreError, SourceCursor, SourceRecordDigest,
};
use crate::session_bundle::{NeutralSessionBundle, SessionBundleError};

pub(crate) struct InspectSource {
    run_id: RunId,
    cursor: SourceCursor,
}

impl InspectSource {
    pub(crate) fn from_bundle(bundle: &NeutralSessionBundle) -> Result<Self, RunStoreError> {
        let cursor = SourceCursor::new(
            bundle.source().provider(),
            bundle.source().session_ref_private(),
            bundle.source().repository_digest(),
            bundle
                .records()
                .iter()
                .map(|record| {
                    SourceRecordDigest::with_role(
                        record.id(),
                        record.content_digest(),
                        record.role(),
                    )
                })
                .collect(),
        )?;
        let mut digest = Sha256::new();
        digest.update(b"driftctl.inspect-run.v1\0");
        digest.update(provider_name(cursor.provider()).as_bytes());
        digest.update([0]);
        digest.update(cursor.session_locator_private().as_bytes());
        digest.update([0]);
        digest.update(cursor.repository_digest().as_bytes());
        let hex = format!("{:x}", digest.finalize());
        let run_id = RunId::parse(format!("run-{}", &hex[..24]))?;
        Ok(Self { run_id, cursor })
    }

    pub(crate) fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub(crate) fn cursor(&self) -> &SourceCursor {
        &self.cursor
    }

    pub(crate) fn delta_bundle(
        &self,
        full: &NeutralSessionBundle,
        accepted_record_count: usize,
    ) -> Result<NeutralSessionBundle, SessionBundleError> {
        NeutralSessionBundle::from_records_with_native_goal(
            full.source().provider(),
            full.source().session_ref_private(),
            full.source().repository_digest(),
            full.native_goal().clone(),
            full.records()[accepted_record_count..].to_vec(),
        )
    }

    pub(crate) fn open(
        &self,
        repository: &Path,
    ) -> Result<Option<LockedInspectRun>, RunStoreError> {
        match RunStore::open_default(repository, self.run_id.as_str()) {
            Ok(store) => {
                let recovered = store.recover()?;
                Ok(Some(LockedInspectRun { store, recovered }))
            }
            Err(RunStoreError::RunNotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn create(
        &self,
        repository: &Path,
        history: &crate::intent_history::History,
        projection: &crate::projection::ActiveProjection,
    ) -> Result<RunStore, RunStoreError> {
        RunStore::create_with_source_cursor(
            RunStore::default_state_root()?,
            repository,
            self.run_id.as_str(),
            history,
            projection,
            &self.cursor,
        )
    }

    /// Only repairs the narrow first-create interruption: every event must be
    /// an initial resolver transition and every accepted source record must
    /// match the immutable history exactly.
    pub(crate) fn can_bind_unbound_initial_history(&self, history: &History) -> bool {
        let mut observed = BTreeMap::new();
        for record in history.records() {
            let sources = match &record.event {
                Event::RunStarted { goal } => goal.source_refs.iter().collect::<Vec<_>>(),
                Event::RequirementAdded {
                    item,
                    approval: None,
                }
                | Event::RequirementSuperseded {
                    replacement: item,
                    approval: None,
                    ..
                } => item.introduced_by.iter().collect(),
                Event::RequirementWithdrawn {
                    source_refs,
                    approval: None,
                    ..
                } => source_refs.iter().collect(),
                Event::ConflictRaised {
                    conflict,
                    approval: None,
                } => {
                    let mut sources = conflict.source_refs.iter().collect::<Vec<_>>();
                    sources.extend(
                        conflict
                            .alternatives
                            .iter()
                            .flat_map(|alternative| alternative.source_refs.iter()),
                    );
                    sources
                }
                _ => return false,
            };
            for source in sources {
                if !record_source(&mut observed, source, &self.cursor) {
                    return false;
                }
            }
        }
        let authoritative_records = self
            .cursor
            .accepted_records()
            .iter()
            .filter(|record| matches!(record.role(), crate::intent_history::SourceRole::User))
            .collect::<Vec<_>>();
        observed.len() == authoritative_records.len()
            && authoritative_records.iter().all(|record| {
                observed
                    .get(record.id())
                    .is_some_and(|digest| *digest == record.content_digest())
            })
    }
}

pub(crate) struct LockedInspectRun {
    pub(crate) store: RunStore,
    pub(crate) recovered: RecoveredRun,
}

fn provider_name(provider: &SourceProvider) -> &'static str {
    match provider {
        SourceProvider::Codex => "codex",
        SourceProvider::Bundle => "bundle",
    }
}

fn record_source<'a>(
    observed: &mut BTreeMap<&'a str, &'a str>,
    source: &'a SourceRef,
    cursor: &SourceCursor,
) -> bool {
    if &source.provider != cursor.provider()
        || source.session_private() != cursor.session_locator_private()
    {
        return false;
    }
    observed
        .insert(source.record.as_str(), source.content_digest.as_str())
        .is_none_or(|digest| digest == source.content_digest)
}
