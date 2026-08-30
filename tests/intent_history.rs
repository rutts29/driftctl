use driftctl::intent_history::{
    Approval, Conflict, ConflictAlternative, ConflictResolution, Event, EventRecord, EvidenceRef,
    EvidenceState, GoalRevision, History, HistoryError, IntentId, IntentItem, IntentKind,
    IntentLifecycle, SourceProvider, SourceRef, SourceRole,
};

fn source(role: SourceRole, record: &str) -> SourceRef {
    SourceRef::new(
        SourceProvider::Bundle,
        "private-session",
        record,
        role,
        format!("sha256:{record}"),
    )
}

fn user_source(record: &str) -> SourceRef {
    source(SourceRole::User, record)
}

fn assistant_source(record: &str) -> SourceRef {
    source(SourceRole::Assistant, record)
}

fn approval(record: &str) -> Approval {
    Approval::new(user_source(record))
}

fn goal(text: &str, record: &str) -> GoalRevision {
    GoalRevision::new(1, text, vec![user_source(record)])
}

fn item(id: &str, text: &str, record: &str) -> IntentItem {
    IntentItem::new(
        IntentId::new(id),
        IntentKind::Constraint,
        text,
        vec![user_source(record)],
    )
}

fn evidence(id: &str, record: &str) -> EvidenceRef {
    EvidenceRef::new(
        id,
        "the requirement was checked",
        vec![assistant_source(record)],
    )
}

#[test]
fn v2_events_replay_to_the_same_state_and_round_trip_as_json() {
    let mut history = History::new(goal("Ship the MVP", "goal-1")).expect("start history");
    history
        .append(Event::RequirementAdded {
            item: item("i-1", "Keep the source read-only", "intent-1"),
            approval: None,
        })
        .expect("add requirement");

    let mut replacement = item("i-2", "Keep the source strictly read-only", "intent-2");
    replacement.supersedes = vec![IntentId::new("i-1")];
    history
        .append(Event::RequirementSuperseded {
            previous_id: IntentId::new("i-1"),
            replacement,
            approval: None,
        })
        .expect("supersede requirement");
    history
        .append(Event::EvidenceAttached {
            intent_id: IntentId::new("i-2"),
            evidence: evidence("e-1", "check-1"),
        })
        .expect("attach evidence");

    let json = history.to_json().expect("serialize history");
    let restored = History::from_json(&json).expect("deserialize history");
    assert_eq!(history.records(), restored.records());
    assert_eq!(history.snapshot(), restored.snapshot());
    assert_eq!(
        restored.intent("i-1").expect("old item").lifecycle,
        IntentLifecycle::Superseded
    );
    assert_eq!(
        restored.intent("i-2").expect("replacement").evidence_state,
        EvidenceState::Satisfied
    );
}

#[test]
fn provenance_is_required_and_assistant_or_tool_intent_needs_explicit_approval() {
    let mut history = History::new(goal("Ship the MVP", "goal-1")).expect("start history");
    let mut no_provenance = item("i-1", "must be true", "intent-1");
    no_provenance.introduced_by.clear();
    assert!(matches!(
        history.append(Event::RequirementAdded {
            item: no_provenance,
            approval: None,
        }),
        Err(HistoryError::MissingSourceRefs { .. })
    ));

    let assistant_item = IntentItem::new(
        IntentId::new("i-2"),
        IntentKind::Outcome,
        "assistant suggestion is not authority",
        vec![assistant_source("assistant-1")],
    );
    assert!(matches!(
        history.append(Event::RequirementAdded {
            item: assistant_item.clone(),
            approval: None,
        }),
        Err(HistoryError::UnauthorizedSource { .. })
    ));
    history
        .append(Event::RequirementAdded {
            item: assistant_item,
            approval: Some(approval("operator-1")),
        })
        .expect("explicit operator approval authorizes assistant-derived intent");
}

#[test]
fn lifecycle_and_evidence_state_are_independent() {
    let mut history = History::new(goal("Ship the MVP", "goal-1")).expect("start history");
    history
        .append(Event::RequirementAdded {
            item: item("i-1", "preserve this behavior", "intent-1"),
            approval: None,
        })
        .expect("add requirement");
    history
        .append(Event::EvidenceAttached {
            intent_id: IntentId::new("i-1"),
            evidence: evidence("e-1", "check-1"),
        })
        .expect("attach evidence");
    history
        .append(Event::RequirementWithdrawn {
            intent_id: IntentId::new("i-1"),
            source_refs: vec![user_source("withdraw-1")],
            approval: None,
        })
        .expect("withdraw requirement");
    let withdrawn = history.intent("i-1").expect("withdrawn item");
    assert_eq!(withdrawn.lifecycle, IntentLifecycle::Withdrawn);
    assert_eq!(withdrawn.evidence_state, EvidenceState::Satisfied);

    history
        .append(Event::EvidenceInvalidated {
            intent_id: IntentId::new("i-1"),
            evidence_id: "e-1".into(),
            source_refs: vec![assistant_source("check-2")],
            reason: "the check was run against the wrong revision".into(),
        })
        .expect("invalidate evidence");
    let reopened = history.intent("i-1").expect("reopened item");
    assert_eq!(reopened.lifecycle, IntentLifecycle::Withdrawn);
    assert_eq!(reopened.evidence_state, EvidenceState::Reopened);
}

#[test]
fn conflicts_block_until_an_authorized_resolution() {
    let mut history = History::new(goal("Ship the MVP", "goal-1")).expect("start history");
    history
        .append(Event::RequirementAdded {
            item: item("i-1", "retain the API", "intent-1"),
            approval: None,
        })
        .expect("add requirement");
    let conflict = Conflict::new(
        "c-1",
        vec![IntentId::new("i-1")],
        vec![
            ConflictAlternative::new("keep", "keep the API", vec![user_source("alt-1")]),
            ConflictAlternative::new("replace", "replace the API", vec![user_source("alt-2")]),
        ],
        vec![user_source("conflict-1")],
    );
    history
        .append(Event::ConflictRaised {
            conflict,
            approval: None,
        })
        .expect("raise conflict");
    assert_eq!(
        history.intent("i-1").expect("conflicted item").lifecycle,
        IntentLifecycle::Conflicted
    );
    assert_eq!(history.open_conflicts().len(), 1);

    assert!(
        history
            .append(Event::ConflictResolved {
                conflict_id: "c-1".into(),
                resolution: ConflictResolution::new(
                    "keep",
                    vec![assistant_source("assistant-resolution")],
                    None,
                ),
            })
            .is_err()
    );
    history
        .append(Event::ConflictResolved {
            conflict_id: "c-1".into(),
            resolution: ConflictResolution::new(
                "keep",
                vec![assistant_source("assistant-resolution")],
                Some(approval("operator-resolution")),
            ),
        })
        .expect("authorized resolution");
    assert_eq!(history.open_conflicts().len(), 0);
    assert_eq!(
        history.intent("i-1").expect("resolved item").lifecycle,
        IntentLifecycle::Active
    );
    assert_eq!(
        history.intent("i-1").expect("resolved item").text,
        "keep the API"
    );
}

#[test]
fn replay_rejects_unknown_versions_gaps_duplicate_ids_impossible_transitions_and_tail_events() {
    let start = EventRecord::new(
        1,
        Event::RunStarted {
            goal: goal("Ship the MVP", "goal-1"),
        },
    );
    let add = EventRecord::new(
        2,
        Event::RequirementAdded {
            item: item("i-1", "must remain true", "intent-1"),
            approval: None,
        },
    );

    let mut unknown_version = vec![start.clone()];
    unknown_version[0].schema_version = 999;
    assert!(matches!(
        History::replay(unknown_version),
        Err(HistoryError::UnsupportedSchemaVersion { .. })
    ));

    assert!(matches!(
        History::replay(vec![start.clone(), EventRecord::new(3, add.event.clone())]),
        Err(HistoryError::SequenceGap { .. })
    ));

    assert!(History::replay(vec![start.clone(), add.clone(), add.clone()]).is_err());

    let impossible = EventRecord::new(
        2,
        Event::RequirementWithdrawn {
            intent_id: IntentId::new("missing"),
            source_refs: vec![user_source("withdraw-1")],
            approval: None,
        },
    );
    assert!(matches!(
        History::replay(vec![start.clone(), impossible]),
        Err(HistoryError::UnknownIntent { .. })
    ));

    let satisfy = EventRecord::new(
        3,
        Event::EvidenceAttached {
            intent_id: IntentId::new("i-1"),
            evidence: evidence("e-1", "check-1"),
        },
    );
    let close = EventRecord::new(
        4,
        Event::RunClosed {
            source_refs: vec![user_source("close-1")],
            approval: None,
        },
    );
    let after_close = EventRecord::new(
        5,
        Event::EvidenceAttached {
            intent_id: IntentId::new("i-1"),
            evidence: evidence("e-2", "check-2"),
        },
    );
    assert!(matches!(
        History::replay(vec![start, add, satisfy, close, after_close]),
        Err(HistoryError::EventAfterClosure)
    ));
}

#[test]
fn goal_revisions_are_monotonic_and_source_linked() {
    let mut history = History::new(goal("Ship the MVP", "goal-1")).expect("start history");
    let mut revised = GoalRevision::new(
        2,
        "Ship the smaller MVP",
        vec![assistant_source("assistant")],
    );
    revised.supersedes_revision = Some(1);
    assert!(
        history
            .append(Event::GoalRevised {
                goal: revised.clone(),
            })
            .is_err()
    );

    revised.approval = Some(approval("operator-goal"));
    history
        .append(Event::GoalRevised { goal: revised })
        .expect("approved goal revision");
    assert_eq!(history.goal().text, "Ship the smaller MVP");

    let mut stale = GoalRevision::new(2, "another goal", vec![user_source("goal-3")]);
    stale.supersedes_revision = Some(2);
    assert!(history.append(Event::GoalRevised { goal: stale }).is_err());
}
