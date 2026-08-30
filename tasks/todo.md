# Acceptance Matrix: Same-Session Keeper

## Integration and Isolation

- [x] K01 install/remove validates the packaged plugin, changes only Driftctl-owned registration/files, and preserves pre-existing hooks/config values.
- [x] K02 integration status blocks actionably when the binary, hook feature, trust, or plugin is unavailable.
- [x] K03 an unenrolled hook is a strict no-op: no output, state write, model call, warning, block, or context.
- [x] K04 attach requires an exact existing session ID, is idempotent, and creates nothing for an unknown ID.
- [x] K05 attached sessions remain isolated across same-repository, cross-repository, and concurrent invocations; each has distinct locks, state, proposals, and cursors.
- [x] K06 detach removes only the exact enrollment; restart after detach remains a strict no-op.
- [x] K07 integration and session state enforce private modes and reject symlinked/insecure paths.

## Semantic Prompt Path

- [x] K08 additive and non-steering prompts preserve source accounting, inject active intent, and grant no authority to assistant/tool records.
- [x] K09 supersession and withdrawal remain auditable while inactive intent leaves injected context.
- [x] K10 ambiguity persists source-linked alternatives and blocks before model execution.
- [x] K11 invalid keeper output or projection overflow leaves accepted state unchanged and blocks with retry/detach.
- [x] K12 duplicate delivery is idempotent; stale source heads and projection revisions cannot commit.

## Lifecycle Recovery

- [x] K13 startup/resume recovers attachment, reconciles missed records exactly once, and injects the current projection.
- [x] K14 PreCompact flushes accepted state; SessionStart(compact) re-injects it before immediate continuation.
- [x] K15 killed keeper or interrupted pending write recovers from durable projection plus verified tail without inventing state.

## Conflict and Native Goal

- [x] K16 matching goals cause no mutation; conflict blocks; reject/edit preserves the current native goal and invalidates stale approval.
- [x] K17 approved goal change binds exact session, projection revision, source head, and goal digest; mismatch requires an operator-executed same-session `/goal` handoff; read-only verification must match exactly.
- [x] K18 wrong-session, stale, replayed, concurrent, or failed approval cannot mutate any native goal through Driftctl.

## Production-Shaped Acceptance

- [ ] K19 checksum-installed CLI completes one real lifecycle: attach existing session, inject prompt, isolate a control session, restart/resume, compact, block conflict, resolve, detach; source worktree and pre-existing guidance/config values remain intact.
- [x] K20 one targeted Daybreak pass completed; its sole release blocker was removed and covered by the final regression and full gate.
