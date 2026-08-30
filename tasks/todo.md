# Acceptance Matrix: Same-Session Keeper

## Integration and Isolation

- [ ] K01 install/remove validates the packaged plugin, changes only Driftctl-owned registration/files, and preserves pre-existing hooks/config values.
- [ ] K02 integration status blocks actionably when the binary, hook feature, trust, or plugin is unavailable.
- [ ] K03 an unenrolled hook is a strict no-op: no output, state write, model call, warning, block, or context.
- [ ] K04 attach requires an exact existing session ID, is idempotent, and creates nothing for an unknown ID.
- [ ] K05 attached sessions remain isolated across same-repository, cross-repository, and concurrent invocations; each has distinct locks, state, proposals, and cursors.
- [ ] K06 detach removes only the exact enrollment; restart after detach remains a strict no-op.
- [ ] K07 integration and session state enforce private modes and reject symlinked/insecure paths.

## Semantic Prompt Path

- [ ] K08 additive and non-steering prompts preserve source accounting, inject active intent, and grant no authority to assistant/tool records.
- [ ] K09 supersession and withdrawal remain auditable while inactive intent leaves injected context.
- [ ] K10 ambiguity persists source-linked alternatives and blocks before model execution.
- [ ] K11 invalid keeper output or projection overflow leaves accepted state unchanged and blocks with retry/detach.
- [ ] K12 duplicate delivery is idempotent; stale source heads and projection revisions cannot commit.

## Lifecycle Recovery

- [ ] K13 startup/resume recovers attachment, reconciles missed records exactly once, and injects the current projection.
- [ ] K14 PreCompact flushes accepted state; SessionStart(compact) re-injects it before immediate continuation.
- [ ] K15 killed keeper or interrupted pending write recovers from durable projection plus verified tail without inventing state.

## Conflict and Native Goal

- [ ] K16 matching goals cause no mutation; conflict blocks; reject/edit preserves the current native goal and invalidates stale approval.
- [ ] K17 approved goal change binds exact session, projection revision, source head, and goal digest; get/clear/set/get must read back exactly.
- [ ] K18 wrong-session, stale, replayed, partial, or failed approval cannot mutate any native goal.

## Production-Shaped Acceptance

- [ ] K19 checksum-installed CLI completes one real lifecycle: attach existing session, inject prompt, isolate a control session, restart/resume, compact, block conflict, resolve, detach; source worktree and pre-existing guidance/config values remain intact.
- [ ] K20 one targeted Daybreak review finds no unresolved release blocker in the shipped flow.
