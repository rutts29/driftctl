use driftctl::intent_history::{
    Approval, Conflict, ConflictAlternative, ConflictResolution, Event, EvidenceRef, GoalRevision,
    History, IntentId, IntentItem, IntentKind, SourceProvider, SourceRef, SourceRole,
};
use driftctl::projection::{ClosureBlockerKind, ProjectionConfig, project};

fn source(role: SourceRole, record: &str) -> SourceRef {
    SourceRef::new(
        SourceProvider::Bundle,
        "projection-session",
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

fn goal(text: &str) -> GoalRevision {
    GoalRevision::new(1, text, vec![user_source("goal")])
}

fn item(id: &str, kind: IntentKind, text: &str, record: &str) -> IntentItem {
    IntentItem::new(IntentId::new(id), kind, text, vec![user_source(record)])
}

fn evidence(id: &str) -> EvidenceRef {
    EvidenceRef::new(
        id,
        "the requirement was verified",
        vec![assistant_source("evidence")],
    )
}

fn conflict(id: &str, intent_id: &str) -> Conflict {
    Conflict::new(
        id,
        vec![IntentId::new(intent_id)],
        vec![
            ConflictAlternative::new("a", "choose A", vec![user_source("alternative-a")]),
            ConflictAlternative::new("b", "choose B", vec![user_source("alternative-b")]),
        ],
        vec![user_source("conflict")],
    )
}

fn history_with_items(items: Vec<IntentItem>) -> History {
    let mut history = History::new(goal("ship the work")).expect("start history");
    for item in items {
        history
            .append(Event::RequirementAdded {
                item,
                approval: None,
            })
            .expect("add requirement");
    }
    history
}

#[test]
fn superseded_and_withdrawn_items_are_excluded_without_mutating_history() {
    let mut history = history_with_items(vec![item(
        "old",
        IntentKind::Outcome,
        "old outcome",
        "old-source",
    )]);
    let mut replacement = item("new", IntentKind::Outcome, "new outcome", "new-source");
    replacement.supersedes = vec![IntentId::new("old")];
    history
        .append(Event::RequirementSuperseded {
            previous_id: IntentId::new("old"),
            replacement,
            approval: None,
        })
        .expect("supersede item");
    history
        .append(Event::RequirementAdded {
            item: item(
                "withdrawn",
                IntentKind::Constraint,
                "remove me",
                "withdrawn-source",
            ),
            approval: None,
        })
        .expect("add withdrawn item");
    history
        .append(Event::RequirementWithdrawn {
            intent_id: IntentId::new("withdrawn"),
            source_refs: vec![user_source("withdraw")],
            approval: None,
        })
        .expect("withdraw item");

    let records_before = history.records().to_vec();
    let projection = project(&history, ProjectionConfig::new(16 * 1024)).expect("project");

    assert_eq!(projection.frontier.len(), 1);
    assert_eq!(projection.frontier[0].id.as_str(), "new");
    assert!(
        !projection
            .frontier
            .iter()
            .any(|item| item.id.as_str() == "old" || item.id.as_str() == "withdrawn")
    );
    assert_eq!(history.records(), records_before.as_slice());
    assert!(!history.intent("old").expect("old history item").is_active());
    assert!(
        !history
            .intent("withdrawn")
            .expect("withdrawn history item")
            .is_active()
    );
}

#[test]
fn satisfied_items_are_preserved_and_invalidated_evidence_reopens_frontier() {
    let mut history = history_with_items(vec![
        item(
            "constraint",
            IntentKind::Constraint,
            "keep this constraint",
            "constraint-source",
        ),
        item(
            "invariant",
            IntentKind::Invariant,
            "keep this invariant",
            "invariant-source",
        ),
        item(
            "outcome",
            IntentKind::Outcome,
            "keep this outcome",
            "outcome-source",
        ),
        item(
            "scope",
            IntentKind::Scope,
            "keep this scope",
            "scope-source",
        ),
    ]);
    for id in ["constraint", "invariant", "outcome", "scope"] {
        history
            .append(Event::EvidenceAttached {
                intent_id: IntentId::new(id),
                evidence: evidence(&format!("e-{id}")),
            })
            .expect("attach evidence");
    }

    let projection = project(&history, 16 * 1024).expect("project satisfied items");
    assert_eq!(
        projection
            .preserve
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["constraint", "invariant", "outcome", "scope"]
    );
    assert!(projection.frontier.is_empty());

    history
        .append(Event::EvidenceInvalidated {
            intent_id: IntentId::new("outcome"),
            evidence_id: "e-outcome".into(),
            source_refs: vec![assistant_source("invalidated")],
            reason: "verification used a stale checkout".into(),
        })
        .expect("invalidate evidence");
    let reopened = project(&history, 16 * 1024).expect("project reopened item");
    assert_eq!(reopened.preserve.len(), 3);
    assert_eq!(reopened.frontier.len(), 1);
    assert_eq!(reopened.frontier[0].id.as_str(), "outcome");
    assert_eq!(reopened.frontier[0].evidence[0].id.as_str(), "e-outcome");
}

#[test]
fn validation_items_are_kept_in_validation_and_open_conflicts_block_closure() {
    let mut history = history_with_items(vec![
        item(
            "check",
            IntentKind::Validation,
            "run the integration check",
            "check-source",
        ),
        item(
            "conflicted",
            IntentKind::Constraint,
            "choose one compatible API",
            "conflicted-source",
        ),
    ]);
    history
        .append(Event::ConflictRaised {
            conflict: conflict("c-1", "conflicted"),
            approval: None,
        })
        .expect("raise conflict");

    let projection = project(&history, 16 * 1024).expect("project conflict");
    assert_eq!(projection.validation.len(), 1);
    assert_eq!(projection.validation[0].id.as_str(), "check");
    assert!(projection.frontier.is_empty());
    assert_eq!(projection.conflicts.len(), 1);
    assert_eq!(projection.conflicts[0].id.as_str(), "c-1");
    assert!(projection.closure.is_blocked());
    assert!(
        projection
            .closure
            .blockers
            .iter()
            .any(|blocker| blocker.kind == ClosureBlockerKind::Conflict)
    );

    history
        .append(Event::ConflictResolved {
            conflict_id: "c-1".into(),
            resolution: ConflictResolution::new(
                "a",
                vec![assistant_source("resolution")],
                Some(approval("operator")),
            ),
        })
        .expect("resolve conflict");
    let resolved = project(&history, 16 * 1024).expect("project resolved conflict");
    assert!(resolved.conflicts.is_empty());
    assert!(
        !resolved
            .closure
            .blockers
            .iter()
            .any(|blocker| blocker.kind == ClosureBlockerKind::Conflict)
    );
}

#[test]
fn replaying_the_same_history_produces_identical_projection_and_order() {
    let mut history = history_with_items(vec![
        item("z", IntentKind::Outcome, "z work", "z-source"),
        item("a", IntentKind::Outcome, "a work", "a-source"),
        item("m", IntentKind::Validation, "m check", "m-source"),
    ]);
    history
        .append(Event::EvidenceAttached {
            intent_id: IntentId::new("z"),
            evidence: evidence("e-z"),
        })
        .expect("attach evidence");

    let replayed = History::replay(history.records().iter().cloned()).expect("replay");
    let first = project(&history, 16 * 1024).expect("project first history");
    let second = project(&replayed, 16 * 1024).expect("project replayed history");
    assert_eq!(first, second);
    assert_eq!(
        first
            .frontier
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a"]
    );
    assert_eq!(
        first
            .validation
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m"]
    );
    assert_eq!(first.rendered_bytes(), first.rendered_prompt().len());
}

#[test]
fn rendered_budget_accepts_exact_boundary_and_reports_structured_overflow_without_truncation() {
    let history = history_with_items(vec![item(
        "large",
        IntentKind::Outcome,
        "a long requirement that must remain wholly represented",
        "large-source",
    )]);
    let unbounded = project(&history, usize::MAX).expect("unbounded projection");
    let exact_budget = unbounded.rendered_bytes();
    let exact = project(&history, ProjectionConfig::new(exact_budget)).expect("exact boundary");
    assert!(!exact.overflow.is_overflow());
    assert_eq!(exact.rendered_bytes(), exact_budget);

    let overflow = project(&history, ProjectionConfig::new(exact_budget - 1)).expect("overflow");
    assert!(overflow.overflow.is_overflow());
    assert_eq!(overflow.overflow.bytes, exact_budget);
    assert_eq!(overflow.overflow.budget, exact_budget - 1);
    assert_eq!(overflow.overflow.item_count, 1);
    assert_eq!(overflow.frontier.len(), 1);
    assert_eq!(
        overflow.frontier[0].text,
        history.intent("large").unwrap().text
    );
    assert!(
        overflow
            .closure
            .blockers
            .iter()
            .any(|blocker| blocker.kind == ClosureBlockerKind::ProjectionOverflow)
    );
}
