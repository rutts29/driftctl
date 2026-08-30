# driftctl workflow

- Product, hook, session, ledger, projection, conflict, goal, or completion work: read `SPEC.md` and `ARCHITECTURE.md`; the same-session flow is the hard boundary.
- Implementation work: read only the current slice in `tasks/plan.md` and its test IDs in `tasks/todo.md`.
- Behavior changes: RED → GREEN → REFACTOR; preserve every existing test and failure artifact.
- Slice completion: focused tests plus the applicable installed-entrypoint checkpoint; mocks cannot close a process boundary.
- Session isolation: key every enrollment and writable state path by exact provider/session identity; unenrolled hooks are strict no-ops.
- Keeper authority: model proposes; deterministic validation commits; operator resolves ambiguity and native-goal change.
- Integration: add only Driftctl-owned plugin files/registration; preserve user hooks, config values, guidance, skills, permissions, credentials, and worktrees.
- Style: rustfmt, explicit typed state, small named helpers, stable domain terms, behavior-first process tests, no unsafe code.
- Reviews: one targeted Daybreak release-blocker pass after the installed MVP works; fix blockers, verify once, ship.
