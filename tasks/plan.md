# Implementation Plan: Same-Session Keeper

## Boundary

- Contract: `SPEC.md`.
- Runtime: `ARCHITECTURE.md`.
- Acceptance matrix: `tasks/todo.md`.
- Existing checkpoint/fork code remains available; it does not define completion.
- Scope additions require a concrete failure against the locked flow.

## Build Order

```text
P0 contract/tests
  → P1 installed hook handshake
  → P2 exact-session enrollment isolation
  → P3 semantic prompt path
  → P4 restart/resume/compaction recovery
  → P5 native-goal approval
  → P6 real E2E/package
```

## Slices

| ID | Slice | Depends | Proof |
|---|---|---|---|
| P0 | Replace drifted contract; freeze test IDs | — | docs agree; every requirement maps to test |
| P1 | Package hooks; handle one real `UserPromptSubmit` | P0 | installed plugin injects sentinel context into an exact disposable session |
| P2 | `integrate`, `attach`, `status`, `detach`; exact-session enrollment | P1 | two same-repo sessions plus unenrolled session remain isolated |
| P3 | Reconcile → keeper proposal → validate → commit → inject/block | P2 | additive, supersession, conflict, invalid-output process tests |
| P4 | `Stop`, `PreCompact`, `SessionStart`; recovery/idempotence | P3 | restart, missed records, worker kill, native compaction E2E |
| P5 | Operator conflict and native-goal resolution | P4 | exact-session clear/set/read-back; wrong/stale approval cannot mutate |
| P6 | Installed real-session acceptance and private package | P5 | full attached lifecycle passes; one Daybreak release-blocker review |

## Checkpoints

### C1 — Hook boundary

- P1 installed entrypoint passes.
- Existing Codex hook definitions remain intact.
- Unenrolled hook invocation is a strict no-op.

### C2 — Attached prompt

- P2–P3 pass through installed binary.
- Same-repository sessions remain isolated.
- Conflict blocks before model execution.

### C3 — Continuity

- P4–P5 pass through installed binary.
- Resume and compact inject the same effective projection.
- Keeper death loses no accepted ledger state.
- Goal mutation requires exact operator approval and read-back.

### C4 — MVP

- Every `tasks/todo.md` case passes.
- Real attached session completes the whole lifecycle.
- Targeted security gate has no unresolved release blocker.
- Package remains private until operator publication.

## Execution Rules

- RED → GREEN → REFACTOR per test ID.
- Commit each green slice.
- Run focused tests after each change; full suite once after the final code change.
- Run installed-entrypoint rehearsal before advancing a checkpoint.
- One final targeted security review; fix release blockers only; rerun affected and full gates once.
- Preserve all old tests, eval cases, results, and failure records.

## Deferred

- Claude native hooks.
- Homebrew/npm/curl publication.
- Daemon.
- Replacement TUI.
- Automatic merge/push.
- New efficacy evaluation before the attached product works.
