# Capability Map: Drift-resistant Agent Workflow

| Module id | Responsibility | Depends on |
|---|---|---|
| `continuity-core` | Append events, reconstruct the complete goal and current frontier, and decide closure | — |
| `cli` | Expose start, steer, status, resume, and verify commands | `continuity-core` |
| `codex-runner` | Run one Codex workflow while preserving existing repository instructions | `cli` |
| `evaluation` | Run the baseline and workflow on the same cases and score verified completion | `codex-runner` |

Build order: `continuity-core` → `cli` → `codex-runner` → `evaluation`.

Claude Code and other harnesses are portability targets, not requirements for the first working release.
