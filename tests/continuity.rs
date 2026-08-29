use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use driftctl::{ClosureError, Ledger};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("driftctl-{case}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

#[test]
fn steering_survives_reopen_and_blocks_premature_closure() {
    let root = temporary_directory("steering-resume");
    let ledger_path = root.join("ledger.jsonl");

    let mut ledger = Ledger::create(&ledger_path, "Add retry support", [("R1", "Retry once")])
        .expect("create run ledger");
    ledger
        .satisfy("R1", "retry unit test passes")
        .expect("record original requirement evidence");
    let steering_id = ledger
        .steer("Do not retry authentication failures")
        .expect("append steering requirement");
    drop(ledger);

    let mut resumed = Ledger::open(&ledger_path).expect("reopen after interruption");
    let snapshot = resumed.snapshot().expect("fold durable events");

    assert_eq!(snapshot.goal(), "Add retry support");
    assert_eq!(
        snapshot.unresolved_requirement_ids(),
        std::slice::from_ref(&steering_id)
    );
    assert_eq!(
        resumed.close(),
        Err(ClosureError::UnresolvedRequirements(vec![
            steering_id.clone()
        ]))
    );

    resumed
        .satisfy(
            &steering_id,
            "integration test excludes authentication failures",
        )
        .expect("record steering evidence");
    assert!(resumed.close().is_ok());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
