use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use driftctl::intent_history::{
    Event, GoalRevision, History, IntentId, IntentItem, IntentKind, SourceProvider, SourceRef,
    SourceRole,
};
use driftctl::projection::{ActiveProjection, project};
use driftctl::run_store::{
    RunId, RunStore, RunStoreError, SourceCursor, SourceCursorComparison, SourceRecordDigest,
    repository_id,
};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "driftctl-run-store-{case}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn source(record: &str) -> SourceRef {
    SourceRef::new(
        SourceProvider::Bundle,
        "private-session",
        record,
        SourceRole::User,
        format!("sha256:{record}"),
    )
}

fn history() -> History {
    History::new(GoalRevision::new(
        1,
        "persist the run",
        vec![source("goal")],
    ))
    .expect("start history")
}

fn item(id: &str, record: &str) -> IntentItem {
    IntentItem::new(
        IntentId::new(id),
        IntentKind::Constraint,
        format!("requirement {id}"),
        vec![source(record)],
    )
}

fn projection(history: &History) -> ActiveProjection {
    project(history, 16 * 1024).expect("project history")
}

fn digest(hex: char) -> String {
    format!("sha256:{}", hex.to_string().repeat(64))
}

fn source_cursor(records: &[(&str, char)]) -> SourceCursor {
    SourceCursor::new(
        SourceProvider::Bundle,
        "private-session-locator",
        digest('f'),
        records
            .iter()
            .map(|(id, hex)| SourceRecordDigest::new(*id, digest(*hex)))
            .collect(),
    )
    .expect("build source cursor")
}

fn create_store(case: &str) -> (PathBuf, PathBuf, RunStore, History) {
    let root = temporary_directory(case);
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create repository");
    let history = history();
    let store = RunStore::create(
        &root,
        &repository,
        "run_01",
        &history,
        &projection(&history),
    )
    .expect("create run store");
    (root, repository, store, history)
}

fn pending_record(history: &History, id: &str) -> driftctl::intent_history::EventRecord {
    driftctl::intent_history::EventRecord::new(
        u64::try_from(history.records().len() + 1).expect("sequence fits"),
        Event::RequirementAdded {
            item: item(id, &format!("source-{id}")),
            approval: None,
        },
    )
}

#[test]
fn creates_and_reopens_an_xdg_local_run_without_storing_the_repository_path() {
    let (root, repository, store, history) = create_store("create-reopen");
    let run_path = store.path().to_path_buf();
    assert!(run_path.starts_with(root.join("repositories")));
    assert!(run_path.join("history").is_dir());
    assert!(run_path.join("projection.json").is_file());
    assert!(run_path.join("pending.jsonl").is_file());
    assert!(!run_path.to_string_lossy().contains("repository"));
    drop(store);

    let reopened = RunStore::open(&root, &repository, "run_01").expect("reopen run store");
    let recovered = reopened.recover().expect("recover committed run");
    assert_eq!(recovered.projection, projection(&history));
    assert!(recovered.pending_delta.is_empty());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn creates_a_fresh_nested_state_root() {
    let outer = temporary_directory("fresh-state-root");
    let state_root = outer.join("missing/state/driftctl");
    let repository = outer.join("repository");
    fs::create_dir(&repository).expect("create repository");
    let history = history();

    let store = RunStore::create(
        &state_root,
        &repository,
        "run_01",
        &history,
        &projection(&history),
    )
    .expect("create run under a previously absent state root");

    assert!(store.path().is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for directory in [
            state_root.as_path(),
            store.path(),
            &store.path().join("history"),
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o077,
                0,
                "private state directory: {}",
                directory.display()
            );
        }
        for file in [
            store.path().join("projection.json"),
            store.path().join("pending.jsonl"),
        ] {
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o077,
                0,
                "private state file: {}",
                file.display()
            );
        }
    }
    fs::remove_dir_all(outer).expect("remove isolated test directory");
}

#[test]
fn derives_a_stable_repository_digest_and_rejects_path_like_run_ids() {
    let root = temporary_directory("identifiers");
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create repository");

    let first = repository_id(&repository).expect("derive repository id");
    let second = repository_id(repository.join(".")).expect("canonical repository id");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!first.contains("repository"));
    assert!(RunId::parse("../other").is_err());
    assert!(RunId::parse(".hidden").is_err());
    assert!(RunId::parse("run_01").is_ok());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn creates_and_recovers_a_private_source_cursor_without_transcript_content() {
    let root = temporary_directory("source-cursor");
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create repository");
    let history = history();
    let cursor = source_cursor(&[("record-1", 'a'), ("record-2", 'b')]);
    let store = RunStore::create_with_source_cursor(
        &root,
        &repository,
        "run_01",
        &history,
        &projection(&history),
        &cursor,
    )
    .expect("create projection then source cursor");
    let source_path = store.path().join("source.json");
    let source = fs::read_to_string(&source_path).expect("read private source cursor");
    let source_json: serde_json::Value =
        serde_json::from_str(&source).expect("parse source cursor");
    assert_eq!(source_json["schema_version"], 1);
    assert_eq!(source_json["provider"], "bundle");
    assert_eq!(source_json["head"], "record-2");
    assert_eq!(
        source_json["accepted_records"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(source.contains("private-session-locator"));
    assert!(source.contains("record-1"));
    assert!(source.contains(&digest('a')));
    assert!(!source.contains("raw transcript content must not persist"));
    assert!(!source.contains("\"content\""));
    let mut legacy_source = source_json.clone();
    for record in legacy_source["accepted_records"]
        .as_array_mut()
        .expect("legacy records")
    {
        record
            .as_object_mut()
            .expect("legacy record")
            .remove("role");
    }
    let legacy_cursor: SourceCursor =
        serde_json::from_value(legacy_source).expect("read pre-role source cursor");
    assert_eq!(legacy_cursor, cursor);
    legacy_cursor.validate().expect("validate pre-role cursor");
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(&source_path)
                .expect("source metadata")
                .permissions()
        ) & 0o077,
        0,
        "source state must not be group/world-readable"
    );

    let recovered = store.recover().expect("recover source cursor");
    assert_eq!(recovered.source_cursor, Some(cursor));

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn source_cursor_compares_the_exact_accepted_prefix() {
    let accepted = source_cursor(&[("record-1", 'a'), ("record-2", 'b')]);
    assert_eq!(
        accepted
            .compare(&source_cursor(&[("record-1", 'a'), ("record-2", 'b')]))
            .expect("compare exact cursor"),
        SourceCursorComparison::Current
    );
    assert_eq!(
        accepted
            .compare(&source_cursor(&[
                ("record-1", 'a'),
                ("record-2", 'b'),
                ("record-3", 'c'),
            ]))
            .expect("compare appended cursor"),
        SourceCursorComparison::NewRecords(vec![SourceRecordDigest::new("record-3", digest('c'))])
    );
    assert!(matches!(
        accepted
            .compare(&source_cursor(&[("record-1", 'a')]))
            .expect("compare stale cursor"),
        SourceCursorComparison::Stale { .. }
    ));
    assert!(matches!(
        accepted
            .compare(&source_cursor(&[("record-1", 'a'), ("record-2", 'c')]))
            .expect("compare rewritten cursor"),
        SourceCursorComparison::Rewrite { .. }
    ));
    let different_session = SourceCursor::new(
        SourceProvider::Bundle,
        "other-private-session",
        digest('f'),
        vec![
            SourceRecordDigest::new("record-1", digest('a')),
            SourceRecordDigest::new("record-2", digest('b')),
        ],
    )
    .expect("build different-session cursor");
    assert_eq!(
        accepted
            .compare(&different_session)
            .expect("compare different session"),
        SourceCursorComparison::SessionMismatch
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_private_source_cursor_and_only_updates_it_after_projection() {
    use std::os::unix::fs::symlink;

    let (root, _repository, store, mut history) = create_store("source-symlink");
    let pending = pending_record(&history, "i-1");
    history
        .append(pending.event.clone())
        .expect("extend history for projection commit");
    store
        .append_pending(pending)
        .expect("append pending record");
    let source_path = store.path().join("source.json");
    let outside = root.join("outside-source.json");
    fs::write(&outside, "outside").expect("write outside source fixture");
    symlink(&outside, &source_path).expect("link source cursor outside state");

    assert!(matches!(
        store.commit_projection_with_source_cursor(
            &projection(&history),
            &source_cursor(&[("record-1", 'a')])
        ),
        Err(RunStoreError::SymlinkRefused { .. })
    ));
    fs::remove_file(&source_path).expect("remove refused source symlink");
    assert_eq!(
        store
            .recover()
            .expect("projection committed before source write")
            .projection,
        projection(&history)
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn rejects_a_second_writer_with_a_deterministic_busy_error() {
    let (root, repository, store, _) = create_store("writer-lock");
    let error = RunStore::open(&root, &repository, "run_01").expect_err("second writer is busy");
    assert_eq!(error, RunStoreError::Busy);
    drop(store);
    RunStore::open(&root, &repository, "run_01").expect("lock is released on drop");

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn lock_holder_subprocess() {
    if std::env::var_os("DRIFTCTL_RUN_STORE_LOCK_CHILD").is_none() {
        return;
    }
    let root =
        PathBuf::from(std::env::var_os("DRIFTCTL_RUN_STORE_STATE_ROOT").expect("state root"));
    let repository =
        PathBuf::from(std::env::var_os("DRIFTCTL_RUN_STORE_REPOSITORY").expect("repository"));
    let ready = PathBuf::from(std::env::var_os("DRIFTCTL_RUN_STORE_READY").expect("ready path"));
    let _store = RunStore::open(root, repository, "run_01").expect("child acquires writer lock");
    fs::write(ready, "locked\n").expect("signal acquired lock");
    loop {
        thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[test]
fn writer_lock_is_released_after_a_holder_process_is_killed() {
    let (root, repository, store, _) = create_store("writer-lock-crash");
    let ready = root.join("lock-holder-ready");
    drop(store);

    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("lock_holder_subprocess")
        .arg("--nocapture")
        .env("DRIFTCTL_RUN_STORE_LOCK_CHILD", "1")
        .env("DRIFTCTL_RUN_STORE_STATE_ROOT", &root)
        .env("DRIFTCTL_RUN_STORE_REPOSITORY", &repository)
        .env("DRIFTCTL_RUN_STORE_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lock holder subprocess");
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready.exists(), "child acquired its lock before the timeout");
    assert_eq!(
        RunStore::open(&root, &repository, "run_01").expect_err("live holder is busy"),
        RunStoreError::Busy
    );

    child.kill().expect("kill lock holder without running Drop");
    child.wait().expect("reap lock holder");
    RunStore::open(&root, &repository, "run_01").expect("killed holder lock is released");

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn recovers_committed_projection_with_an_unrotated_pending_delta_and_ignores_temp_files() {
    let (root, repository, store, history) = create_store("recovery");
    let record = pending_record(&history, "i-1");
    store
        .append_pending(record.clone())
        .expect("append pending record");
    fs::write(
        store.path().join(".projection.json.tmp-interrupted"),
        "not a projection",
    )
    .expect("leave interrupted temp artifact");
    drop(store);

    let reopened = RunStore::open(&root, &repository, "run_01").expect("reopen run store");
    let recovered = reopened.recover().expect("recover after interruption");
    assert_eq!(recovered.projection, projection(&history));
    assert_eq!(recovered.pending_delta, vec![record]);

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn recovers_a_durable_projection_when_its_accepted_delta_was_not_yet_rotated() {
    let (root, repository, store, mut history) = create_store("projection-before-rotation");
    let record = pending_record(&history, "i-1");
    history
        .append(record.event.clone())
        .expect("extend accepted history");
    let accepted = projection(&history);
    store
        .append_pending(record.clone())
        .expect("append pending record");
    fs::write(
        store.path().join("projection.json"),
        serde_json::to_vec(&accepted).expect("serialize durable projection"),
    )
    .expect("simulate completed projection replacement before rotation");
    drop(store);

    let reopened = RunStore::open(&root, &repository, "run_01").expect("reopen interrupted run");
    let recovered = reopened.recover().expect("recover interrupted commit");
    assert_eq!(recovered.projection, accepted);
    assert_eq!(recovered.pending_delta, vec![record]);

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn atomically_replaces_projection_then_rotates_exact_pending_delta_to_immutable_history() {
    let (root, _repository, store, mut history) = create_store("commit-rotate");
    let record = pending_record(&history, "i-1");
    history
        .append(record.event.clone())
        .expect("extend accepted history");
    store
        .append_pending(record.clone())
        .expect("append pending record");

    let accepted = projection(&history);
    store
        .commit_projection(&accepted)
        .expect("commit projection");

    let recovered = store.recover().expect("recover committed projection");
    assert_eq!(recovered.projection, accepted);
    assert!(recovered.pending_delta.is_empty());
    let segments = fs::read_dir(store.path().join("history"))
        .expect("read immutable history")
        .map(|entry| entry.expect("history entry").path())
        .collect::<Vec<_>>();
    assert_eq!(segments.len(), 2);
    let rotated = segments
        .iter()
        .find(|path| {
            fs::read_to_string(path)
                .expect("read segment")
                .contains("i-1")
        })
        .expect("pending delta rotated to a segment");
    assert_eq!(
        fs::read_to_string(rotated).expect("read rotated delta"),
        serde_json::to_string(&record).expect("serialize record") + "\n"
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn refuses_to_overwrite_an_immutable_history_segment() {
    let (root, _repository, store, mut history) = create_store("immutable-history");
    let record = pending_record(&history, "i-1");
    history
        .append(record.event.clone())
        .expect("extend accepted history");
    store.append_pending(record).expect("append pending record");
    let existing_segment = store
        .path()
        .join("history/00000000000000000002-00000000000000000002.jsonl");
    fs::write(&existing_segment, "immutable fixture\n").expect("create conflicting segment");

    assert!(matches!(
        store.commit_projection(&projection(&history)),
        Err(RunStoreError::ImmutableHistorySegment { .. })
    ));
    assert_eq!(
        fs::read_to_string(existing_segment).expect("read immutable fixture"),
        "immutable fixture\n"
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_state_components() {
    use std::os::unix::fs::symlink;

    let state_root = temporary_directory("symlink");
    let repository = state_root.join("repository");
    fs::create_dir(&repository).expect("create repository");
    let outside = state_root.join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    symlink(&outside, state_root.join("repositories")).expect("place symlinked state component");
    let initial_history = history();

    assert!(matches!(
        RunStore::create(
            &state_root,
            &repository,
            "run_01",
            &initial_history,
            &projection(&initial_history)
        ),
        Err(RunStoreError::SymlinkRefused { .. })
    ));
    fs::remove_dir_all(state_root).expect("remove isolated test directory");

    let root = temporary_directory("symlink-projection");
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create repository");
    let history = history();
    let store = RunStore::create(
        &root,
        &repository,
        "run_01",
        &history,
        &projection(&history),
    )
    .expect("create ordinary run");
    let outside = root.join("outside-projection.json");
    fs::write(&outside, "outside").expect("write outside projection");
    fs::remove_file(store.path().join("projection.json")).expect("remove stored projection");
    symlink(&outside, store.path().join("projection.json")).expect("link projection outside state");
    drop(store);

    assert!(matches!(
        RunStore::open(&root, &repository, "run_01"),
        Err(RunStoreError::SymlinkRefused { .. })
    ));

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[cfg(unix)]
#[test]
fn refuses_dangling_symlinked_pending_delta() {
    use std::os::unix::fs::symlink;

    let (root, repository, store, _) = create_store("dangling-pending-symlink");
    let pending = store.path().join("pending.jsonl");
    fs::remove_file(&pending).expect("remove pending delta");
    symlink(root.join("missing-pending.jsonl"), &pending).expect("create dangling pending symlink");
    drop(store);

    assert!(matches!(
        RunStore::open(&root, &repository, "run_01"),
        Err(RunStoreError::SymlinkRefused { .. })
    ));

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn leaves_the_v1_ledger_in_place_without_migrating_it() {
    let root = temporary_directory("v1-coexistence");
    let repository = root.join("repository");
    let legacy = repository.join(".driftctl/ledger.jsonl");
    fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("create legacy directory");
    fs::write(&legacy, "v1 ledger remains untouched\n").expect("write v1 ledger");
    let history = history();

    let store = RunStore::create(
        &root,
        &repository,
        "run_01",
        &history,
        &projection(&history),
    )
    .expect("create v2 run without migration");
    assert!(store.path().starts_with(root.join("repositories")));
    assert_eq!(
        fs::read_to_string(&legacy).expect("read v1 ledger"),
        "v1 ledger remains untouched\n"
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
