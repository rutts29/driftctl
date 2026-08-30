# Representative trajectories

| Role | File | Boundary |
|---|---|---|
| Semantic compactor | `conflict-compactor.sanitized.jsonl` | One real Luna Max proposal from the successful synthetic no-TTY conflict gate; generated with `sanitize_trajectory.py`. |
| Coding continuation | `codex-smoke.sanitized.jsonl` | One normalized synthetic coding turn showing test failure, implementation, passing checks, and terminal usage. |

External graders are deterministic commands, not agents. Raw provider trajectories remain local because they can contain session IDs, paths, or private context. Both committed trajectories require manual review in addition to automated sanitization.
