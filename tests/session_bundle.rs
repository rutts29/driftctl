use driftctl::intent_history::{SourceProvider, SourceRole};
use driftctl::session_bundle::{
    BundleRecord, NativeGoal, NeutralSessionBundle, SessionBundleError,
};

fn record(id: &str, role: SourceRole, content: &str) -> BundleRecord {
    BundleRecord::new(id, role, content).expect("valid bundle record")
}

#[test]
fn round_trips_a_versioned_bundle_without_reordering_records_or_losing_source_refs() {
    let bundle = NeutralSessionBundle::from_records(
        SourceProvider::Codex,
        "private-thread-123",
        "sha256:8d3bb5c6a01d8fa89d979122c3c7f1c7f771b24827b8bc6886b9b7fc059f1f37",
        vec![
            record(
                "turn-1:0",
                SourceRole::User,
                "Add retries.\r\nKeep auth safe.",
            ),
            record(
                "turn-2:0",
                SourceRole::Assistant,
                "I will inspect the client.",
            ),
            record("turn-3:0", SourceRole::User, "Never retry auth failures."),
        ],
    )
    .expect("construct bundle");

    let json = bundle.to_json().expect("serialize bundle");
    let reparsed = NeutralSessionBundle::from_json(&json).expect("strictly parse bundle");

    assert_eq!(reparsed, bundle);
    assert_eq!(
        reparsed
            .records()
            .iter()
            .map(BundleRecord::id)
            .collect::<Vec<_>>(),
        vec!["turn-1:0", "turn-2:0", "turn-3:0"]
    );
    assert_eq!(
        reparsed.records()[0].content(),
        "Add retries.\nKeep auth safe."
    );
    assert_eq!(reparsed.authoritative_records().len(), 2);
    assert_eq!(reparsed.source_refs()[2].record, "turn-3:0");
    assert_eq!(reparsed.source_refs()[2].role, SourceRole::User);
    assert!(reparsed.source().digest().starts_with("sha256:"));
    assert_eq!(reparsed.source().head(), "turn-3:0");
    assert_eq!(reparsed.native_goal(), &NativeGoal::Unknown);
}

#[test]
fn rejects_unknown_schema_and_unknown_or_inconsistent_record_fields() {
    let bundle = NeutralSessionBundle::from_records(
        SourceProvider::Codex,
        "private-session",
        "sha256:8d3bb5c6a01d8fa89d979122c3c7f1c7f771b24827b8bc6886b9b7fc059f1f37",
        vec![record("message-1", SourceRole::User, "Do the thing")],
    )
    .expect("construct valid bundle");
    let valid: serde_json::Value =
        serde_json::from_str(&bundle.to_json().expect("serialize bundle")).expect("valid JSON");

    let invalid_values = [
        json_with(&valid, |value| {
            value["schema_version"] = serde_json::json!(2)
        }),
        json_with(&valid, |value| {
            value["records"][0]["role"] = serde_json::json!("developer")
        }),
        json_with(&valid, |value| {
            value["records"][0]["content_digest"] = serde_json::json!("sha256:0")
        }),
        json_with(&valid, |value| {
            value["records"][0]["id"] = serde_json::json!("")
        }),
        json_with(&valid, |value| {
            value["unexpected"] = serde_json::json!(true)
        }),
    ];
    for invalid in invalid_values {
        assert!(NeutralSessionBundle::from_json(&invalid.to_string()).is_err());
    }

    let duplicate_ids = json_with(&valid, |value| {
        let duplicate = value["records"][0].clone();
        value["records"]
            .as_array_mut()
            .expect("records array")
            .push(duplicate);
    });
    assert!(matches!(
        NeutralSessionBundle::from_json(&duplicate_ids.to_string()),
        Err(SessionBundleError::DuplicateRecordId(_))
    ));
}

fn json_with(
    value: &serde_json::Value,
    change: impl FnOnce(&mut serde_json::Value),
) -> serde_json::Value {
    let mut result = value.clone();
    change(&mut result);
    result
}

#[test]
fn debug_and_sanitized_summary_never_render_private_locator_or_raw_text() {
    let bundle = NeutralSessionBundle::from_records(
        SourceProvider::Codex,
        "private-session-do-not-render",
        "sha256:8d3bb5c6a01d8fa89d979122c3c7f1c7f771b24827b8bc6886b9b7fc059f1f37",
        vec![record(
            "message-private",
            SourceRole::User,
            "private user text must not render",
        )],
    )
    .expect("construct bundle");

    let debug = format!("{bundle:?}");
    let source_refs_debug = format!("{:?}", bundle.source_refs());
    let summary = bundle.sanitized_summary().to_string();
    assert!(!debug.contains("private-session-do-not-render"));
    assert!(!debug.contains("private user text must not render"));
    assert!(!source_refs_debug.contains("private-session-do-not-render"));
    assert!(!summary.contains("private-session-do-not-render"));
    assert!(!summary.contains("private user text must not render"));
}

#[test]
fn round_trips_native_goal_states_and_rejects_invalid_state_text_combinations() {
    let known = NeutralSessionBundle::from_records_with_native_goal(
        SourceProvider::Codex,
        "private-session",
        "sha256:8d3bb5c6a01d8fa89d979122c3c7f1c7f771b24827b8bc6886b9b7fc059f1f37",
        NativeGoal::known("Keep the migration reversible.").expect("valid known goal"),
        vec![record("message-1", SourceRole::User, "Continue the work")],
    )
    .expect("construct known-goal bundle");
    let known_json = known.to_json().expect("serialize known goal");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&known_json).expect("JSON")["native_goal"],
        serde_json::json!({"state": "known", "text": "Keep the migration reversible."})
    );
    assert_eq!(
        NeutralSessionBundle::from_json(&known_json).expect("parse known goal"),
        known
    );
    assert!(!format!("{known:?}").contains("Keep the migration reversible."));

    for goal in [NativeGoal::Absent, NativeGoal::Unknown] {
        let bundle = NeutralSessionBundle::from_records_with_native_goal(
            SourceProvider::Codex,
            "private-session",
            "sha256:8d3bb5c6a01d8fa89d979122c3c7f1c7f771b24827b8bc6886b9b7fc059f1f37",
            goal,
            vec![record("message-1", SourceRole::User, "Continue the work")],
        )
        .expect("construct goal-state bundle");
        assert_eq!(
            NeutralSessionBundle::from_json(&bundle.to_json().expect("serialize bundle"))
                .expect("parse goal-state bundle"),
            bundle
        );
    }

    let valid: serde_json::Value = serde_json::from_str(&known_json).expect("known JSON");
    for invalid in [
        json_with(&valid, |value| {
            value["native_goal"] = serde_json::json!({"state": "known"})
        }),
        json_with(&valid, |value| {
            value["native_goal"] = serde_json::json!({"state": "absent", "text": "must not appear"})
        }),
        json_with(&valid, |value| {
            value["native_goal"] =
                serde_json::json!({"state": "unknown", "text": "must not appear"})
        }),
        json_with(&valid, |value| {
            value["native_goal"] = serde_json::json!({"state": "unsupported"})
        }),
    ] {
        assert!(NeutralSessionBundle::from_json(&invalid.to_string()).is_err());
    }
}
