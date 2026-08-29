# driftctl workflow

- Behavior change: read `SPEC.md`; completion requires every applicable acceptance criterion.
- State, session, projection, conflict, fork, or verifier change: read `ARCHITECTURE.md`; preserve its trust boundaries and state transitions.
- Multi-file implementation: read `tasks/plan.md`; follow dependency gates and keep each slice independently green.
- Evaluation or release change: read `REPRODUCING.md` and `IMPROVEMENT-CHANGELOG.md`; preserve every prior case, result, failed experiment, and stated limitation.
- Adapt proven private-harness behavior when faster; keep this repository independently buildable and runnable.
- Use RED → GREEN → REFACTOR for behavior changes.
- Apply one Rust house style: rustfmt layout, explicit typed state, small named helpers, stable domain terms, behavior-first integration tests, no unsafe code.
- Protect the source: provider sessions, native goals, source worktrees, `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, credentials, and global configuration are read-only inputs.
- Complete a task only after its focused checks and the applicable checkpoint in `tasks/plan.md` pass.
- A checkpoint requires a production-shaped run through the shipped user entrypoint and its real process, filesystem, and permission boundaries; mocks and component tests cannot close it.
- If the real boundary cannot run, mark the checkpoint blocked. Add scope only for a concrete execution failure against the frozen MVP contract.
