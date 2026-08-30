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

## In-Session Plugin UX

- [x] U01 exact `$driftctl on` activates and injects only the invoking session.
- [x] U02 installation, startup, resume, compaction, and ordinary prompts never activate a session.
- [x] U03 near-match or embedded control text never changes enrollment.
- [x] U04 exact `$driftctl status` reports exact-session state without creating enrollment or invoking the keeper.
- [x] U05 exact `$driftctl off` detaches only the invoking session; later hooks stay no-op.
- [x] U06 control prompts remain source-accounted but never become semantic intent, including detach and reactivation.
- [x] U07 installed bundle contains the skill and hooks; packaged binary passes the isolated entrypoint flow without changing the active Codex profile.

## Prospective Paired A/B

- [x] A01 CLI accepts only `ab prepare codex` with one source selection and `ab report --run ... -- <verifier>`; malformed or repeated options fail before mutation.
- [x] A02 prepare resolves one stable source, copies equal candidate checkpoints, creates distinct persisted children with verified common-parent lineage, preserves inherited native goal/policy, starts no turn, and leaves source unchanged.
- [x] A03 prepare creates no enrollment; exact `$driftctl on` enrolls only workflow; baseline, source, and unrelated sessions remain detached.
- [x] A04 private durable experiment state records digests/counts/IDs/CWDs/policy without transcript text; partial failure is explicit and non-runnable.
- [x] A05 report rejects wrong/missing workflow enrollment, enrolled baseline, changed source, wrong child CWD/session, unequal starting digest, or missing verifier.
- [x] A06 one identical external verifier runs independently in both candidates; verifier/candidate mutation fails; same-command retry is cached and a different command cannot overwrite evidence.
- [x] A07 report emits `prospective_paired`, per-arm verified completion, source/enrollment invariants, post-fork record/user-prompt counts, verifier timing, and measured keeper call/token overhead or explicit unavailable fields.
- [x] A08 packaged entrypoint completes a disposable provider rehearsal and one real Codex checkpoint → two forks → baseline detached/workflow explicit-on → report lifecycle without installing into the active Codex profile; temporary test resources are removed after evidence retention.
