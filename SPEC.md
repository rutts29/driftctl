# Product Contract: Same-Session Keeper

## Status

- Locked: 2026-08-30.
- Hard boundary: approved runtime flow in `ARCHITECTURE.md`.
- Current checkpoint/fork workflow: retained as recovery and evaluation support.

## Objective

- User: developer continuing one long-running coding-agent session.
- Outcome: the same session receives its effective goal and steering before every model turn and after compaction or resume.
- Authority: explicit user text and operator approvals.
- Storage: private local source-linked ledger plus bounded active projection.
- Integration: explicit-only `$driftctl` Codex skill plus lifecycle hooks; no replacement TUI, fork, hosted service, or repository guidance change.

## Commands

```text
driftctl integrate codex install
driftctl integrate codex status
driftctl integrate codex remove
$driftctl on
$driftctl status
$driftctl off
driftctl attach codex --session <exact-id>
driftctl status codex --session <exact-id>
driftctl resolve codex --session <exact-id> ...
driftctl detach codex --session <exact-id>
driftctl ab prepare codex (--last | --session <id> [--allow-ancestor-cwd]) [--json]
driftctl ab report --run <ab-run-id> [--json] -- <program> [args...]
```

Internal hook entrypoints:

```text
driftctl hook codex
```

The Codex hook event is read from stdin and validated before enrollment lookup.

## Paired A/B Contract

1. `ab prepare` resolves one persisted source checkpoint before creating either arm.
2. It materializes equal baseline/workflow repositories and creates two distinct persisted Codex forks with the same inherited history, native goal, model, effort, sandbox, and approval policy.
3. Prepare starts no agent turn, creates no enrollment, and leaves source session/worktree unchanged.
4. Baseline continues normally. Workflow continues only after exact `$driftctl on` inside that fork.
5. `ab report` requires baseline detached and workflow attached to its exact candidate repository.
6. One verifier command runs independently against both candidates. Its program is canonicalized from the report invocation directory, must be one regular file outside both candidates, and is content-pinned across arms. Candidate-resolved inputs, verifier mutation, candidate mutation, or nonzero exit fail closed.
7. Repeating report with the same command returns stored evidence; a different command cannot replace it.
8. Primary result: verified completion. Post-checkpoint prompts, verifier time, and keeper calls/tokens are secondary evidence only.
9. Result label: `prospective_paired`. Retrospective session pairing is deferred.

## Runtime Contract

1. Installation loads the explicit-only skill and hooks but creates no enrollment.
2. Exact `$driftctl on` at `UserPromptSubmit` uses Codex-supplied `session_id` and `cwd`, reads that persisted thread and native goal, and creates private session-keyed history, cursor, and projection.
3. The external `attach --session` command reaches the same bootstrap path for diagnostics and compatibility.
4. Hook handlers return immediately with no output when the session is not enrolled.
5. `UserPromptSubmit` reconciles unseen records, folds the current projection with the new authoritative user prompt, validates the keeper proposal, and either:
   - commits and returns the projection as additional developer context; or
   - records a source-linked conflict and blocks before the model runs.
6. Exact `$driftctl status` reads enrollment without keeper reconciliation or state creation.
7. Exact `$driftctl off` removes only the invoking session's enrollment; later hooks become no-ops.
8. `Stop` records non-authoritative assistant/process evidence and advances the observed cursor.
9. `PreCompact` flushes durable state.
10. `SessionStart(startup|resume|compact)` recovers state, reconciles missed records, and injects the active projection before the next model request.

## Semantic Contract

- Immutable history retains original goal, all user steering, transitions, approvals, and source references.
- Active projection contains only:
  - current goal;
  - active invariants and constraints;
  - latest effective steering;
  - unresolved frontier;
  - unresolved conflicts;
  - closure conditions.
- Lifecycle: `active`, `superseded`, `withdrawn`, `conflicted`.
- Superseded and withdrawn items remain auditable and leave active model context.
- Assistant/tool text has no intent authority.
- Keeper output is proposal-only; deterministic validation owns commit authority.
- Projection overflow, invalid proposal, unavailable keeper, or unresolved conflict blocks an attached session.

## Conflict and Goal Contract

- Additive steering: commit after validation.
- Explicit compatible supersession: deactivate replaced intent and commit replacement.
- Ambiguous steering: block for operator resolution.
- Native-goal mismatch: block for operator resolution.
- Goal approval binds exact session ID, projection revision, source head, and proposed text.
- Driftctl never clears or sets the native goal of an attached same session.
- Mismatch returns a manual handoff: operator runs `/goal clear` and `/goal <proposed>` inside that exact session, then reruns approval.
- Approval commits only after read-only exact native-goal and source-head verification.
- Reject, edit, failure, concurrent change, stale approval, replay, or wrong session cannot mutate the native goal through Driftctl.

## Isolation Contract

- Enrollment key: exact provider plus exact session ID; repository identity is validation data, never the enrollment key.
- Attaching session A cannot activate session B, including another session in the same repository.
- Installation, startup, resume, compaction, ordinary prompts, and near-match control text cannot create enrollment.
- Persisted exact control commands are source-accounted system observations, never user task authority.
- Existing hook definitions, config values, `AGENTS.md`, `CLAUDE.md`, skills, permissions, provider authentication, and worktree remain owned by the user.
- Integration adds/removes only Driftctl's isolated plugin registration and files.
- Unenrolled sessions execute no model call, ledger write, warning, block, or injected context.
- State directories/files: private; symlinked or insecure state fails closed.
- Service and telemetry: none.
- Provider use: user's existing authentication and allowance; credentials are never copied.

## Model Policy

- Default keeper: `gpt-5.6-luna`, reasoning `max`.
- Optional: `gpt-5.6-terra`, reasoning `high`; `medium` allowed.
- Sol: explicit advanced override only.
- Security review: Daybreak, once after the installed product flow passes.

## Project Structure

```text
src/                 Rust CLI, state, semantic core, Codex adapter
plugins/             installable Codex lifecycle integration
tests/               unit, process, and installed-entrypoint tests
evals/               retained evaluation fixtures and results
tasks/plan.md         ordered implementation slices
tasks/todo.md         locked acceptance matrix
```

## Code Style

```rust
pub fn handle(event: HookEvent, state: &mut SessionState) -> Result<HookOutput, HookError> {
    let proposal = state.propose(event)?;
    state.validate_and_commit(proposal)
}
```

- Rustfmt layout; explicit typed state; small named helpers; stable domain terms; no unsafe code.
- One module owns each mutation boundary.

## Testing Strategy

- RED → GREEN → REFACTOR for every behavior change.
- Pure state transitions: unit tests.
- Filesystem, lock, App Server, plugin, and hook protocol: process tests.
- Each completed slice: checksum-installed binary through the real process/filesystem/permission boundary.
- Final acceptance: one real attached Codex session across prompt and process restart/resume; production-shaped installed hook boundaries for compaction, conflict, goal approval, isolation, and detach.
- Frozen cases: `tasks/todo.md`; no deletion or weakening.

## Boundaries

- Always: preserve prior tests and evidence; commit green vertical slices; record real failures.
- Ask first: change this contract; add provider scope; publish; change an operator-approved goal.
- Never: infer enrollment; silently resolve conflict; mutate another session; treat model output as authority; claim mocks close an installed boundary.

## Success Criteria

- Every test in `tasks/todo.md` passes.
- Exact `$driftctl on` injects a validated projection into the invoking session before the model runs.
- Restart and resume restore the same attachment and projection; `PreCompact` followed by `SessionStart(compact)` restores it through the installed hook entrypoint.
- Two concurrent sessions in one repository remain isolated.
- Detach restores no-op behavior.
- One production-shaped real-session E2E passes.
- No unresolved release-blocking Daybreak finding remains.

## Non-Goals

- General security harness.
- Background service or daemon.
- Replacement TUI.
- Autonomous code review loop.
- Autonomous merge, push, or publication.
- Claude native adapter in this MVP.
- Statistical efficacy claim before real comparative evidence exists.
- Arbitrary historical-turn slicing; select or create the persisted midpoint session first.
