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
- Integration: opt-in Codex lifecycle hooks; no replacement TUI, fork, hosted service, or repository guidance change.

## Commands

```text
driftctl integrate codex install
driftctl integrate codex status
driftctl integrate codex remove
driftctl attach codex --session <exact-id>
driftctl status codex --session <exact-id>
driftctl resolve codex --session <exact-id> ...
driftctl detach codex --session <exact-id>
```

Internal hook entrypoints:

```text
driftctl hook codex session-start
driftctl hook codex user-prompt
driftctl hook codex pre-compact
driftctl hook codex stop
```

## Runtime Contract

1. `attach` reads the exact persisted Codex thread and native goal through App Server.
2. `attach` creates private session-keyed state, initial source cursor, immutable semantic history, and bounded projection.
3. Hook handlers return immediately with no output when the session is not enrolled.
4. `UserPromptSubmit` reconciles unseen records, folds the current projection with the new authoritative user prompt, validates the keeper proposal, and either:
   - commits and returns the projection as additional developer context; or
   - records a source-linked conflict and blocks before the model runs.
5. `Stop` records non-authoritative assistant/process evidence and advances the observed cursor.
6. `PreCompact` flushes durable state.
7. `SessionStart(startup|resume|compact)` recovers state, reconciles missed records, and injects the active projection before the next model request.
8. `detach` removes only the exact enrollment; later hooks for that session become no-ops.

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
- Approved goal transition binds exact session ID, projection revision, source head, and proposed text.
- Transition order: `goal/get → goal/clear → goal/set → goal/get`.
- Exact read-back is required before unblocking.
- Reject, edit, failure, stale approval, or wrong session cannot change the native goal.

## Isolation Contract

- Enrollment key: exact provider plus exact session ID; repository identity is validation data, never the enrollment key.
- Attaching session A cannot activate session B, including another session in the same repository.
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
- Final acceptance: one real attached Codex session across prompt, conflict, restart/resume, compaction, and detach.
- Frozen cases: `tasks/todo.md`; no deletion or weakening.

## Boundaries

- Always: preserve prior tests and evidence; commit green vertical slices; record real failures.
- Ask first: change this contract; add provider scope; publish; change an operator-approved goal.
- Never: infer enrollment; silently resolve conflict; mutate another session; treat model output as authority; claim mocks close an installed boundary.

## Success Criteria

- Every test in `tasks/todo.md` passes.
- Installed hook injects a validated projection into the exact attached session before the model runs.
- Restart, resume, and native compaction restore the same attachment and projection.
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
