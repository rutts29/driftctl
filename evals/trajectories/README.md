# Representative agent trajectories

| Agent role | Instructions | Trajectory | Kind |
|---|---|---|---|
| Codex development agent | `AGENTS.md`, `SPEC.md`, operator steering | `development-recovery.sanitized.jsonl` | Reconstructed current-session record tied to commits and test output |
| Luna semantic keeper | Resolver contract in `src/semantic_resolver.rs` | `conflict-gate-flow.sanitized.jsonl` and `conflict-compactor.sanitized.jsonl` | Reconstructed flow plus sanitized real provider events |
| Coding continuation | Case contract under `evals/cases/01-steering-retry/` | `codex-smoke.sanitized.jsonl` | Normalized synthetic live run |
| Daybreak security reviewer | Release-blocker-only review instruction in the trajectory | `security-review.sanitized.jsonl` | Reconstructed index tied to red tests and commits |

The reconstructed files are not raw model transcripts. They name the instruction, action, tool result, retry, human checkpoint, and retained commit without inventing private dialogue. Raw development and provider traces stay local because they contain unrelated session content, paths, and IDs.

External verifiers are deterministic commands, not agents. `sanitize_trajectory.py` removes paths and provider IDs, but every published file still requires manual review.
