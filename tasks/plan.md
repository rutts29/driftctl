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
  → P7 explicit in-session plugin controls
  → A0 paired A/B contract
  → A1 goal-preserving idle forks
  → A2 durable prepare/report
  → A3 packaged + real-provider proof
  → H0 historical checkpoint contract
  → H1 exact-turn provider fork
  → H2 historical source snapshot + report
  → H3 packaged + real-provider proof
  → I0 macOS incident regressions
  → I1 truthful namespaced control
  → I2 parent-path preflight
  → I3 isolated installed E2E
```

## Slices

| ID | Slice | Depends | Proof |
|---|---|---|---|
| P0 | Replace drifted contract; freeze test IDs | — | docs agree; every requirement maps to test |
| P1 | Package hooks; handle one real `UserPromptSubmit` | P0 | installed plugin injects sentinel context into an exact disposable session |
| P2 | `integrate`, `attach`, `status`, `detach`; exact-session enrollment | P1 | two same-repo sessions plus unenrolled session remain isolated |
| P3 | Reconcile → keeper proposal → validate → commit → inject/block | P2 | additive, supersession, conflict, invalid-output process tests |
| P4 | `Stop`, `PreCompact`, `SessionStart`; recovery/idempotence | P3 | restart, missed records, worker kill, native compaction E2E |
| P5 | Operator conflict and native-goal resolution | P4 | operator-executed same-session goal handoff; Driftctl read-only verification; wrong/stale approval cannot mutate |
| P6 | Installed real-session acceptance and private package | P5 | full attached lifecycle passes; one Daybreak release-blocker review |
| P7 | `$driftctl on|off|status`; exact-session opt-in UX | P6 | isolated plugin/process tests prove explicit activation, no automatic activation, exact detach, resume/compact continuity, and package contents |
| A0 | Freeze prospective paired experiment contract | P7 | `SPEC.md`, architecture, and A01–A08 agree |
| A1 | Preserve inherited goal while creating two idle persisted forks | A0 | provider process tests prove distinct lineage, equal policy, no turn, no enrollment |
| A2 | `ab prepare codex`; `ab report` | A1 | installed CLI prepares, resumes, enrolls only B, grades both once, persists honest report |
| A3 | Production-shaped A/B proof | A2 | disposable real Codex checkpoint/forks plus packaged entrypoint; active profile untouched |
| H0 | Freeze historical checkpoint contract | A3 | `SPEC.md`, architecture, and H01–H06 agree |
| H1 | Add completed-turn preflight and `lastTurnId` | H0 | adapter/process tests prove exact selection and mutation-free active-turn rejection |
| H2 | Bind an explicit Git commit and prefix-aware report | H1 | installed CLI creates equal historical arms and reports against the exact prefix |
| H3 | Production-shaped historical proof | H2 | disposable real Codex multi-turn session → historical A/B prepare through packaged entrypoint |
| I0 | Freeze macOS incident regressions | H3 | I01–I02 fail against v0.5.0 with captured symptoms |
| I1 | Accept Codex-qualified exact controls | I0 | namespaced on/status/off produce durable exact-session results; near matches stay inert |
| I2 | Fail closed on inherited parent-checkout paths | I1 | unsafe prepare creates no A/B state, workspace, or fork |
| I3 | Isolated installed rehearsal | I2 | packaged entrypoint passes without changing the active Codex profile |

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
- Native-goal mutation remains inside the exact operator-controlled Codex session; Driftctl requires exact read-back.

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
- Retrospective pairing of unrelated sessions.
- Automatic arm execution or adoption.
- Automatic recovery of historical uncommitted files.
