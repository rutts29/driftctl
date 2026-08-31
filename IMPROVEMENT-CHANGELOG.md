# Improvement changelog

This is the judge-facing experiment history. Failed and removed experiments remain visible; detailed machine evidence is under `evals/results/` and in Git history.

| Stage | What changed | Evidence | Decision |
|---|---|---|---|
| 1. Direct baseline | A fresh general-purpose coding agent resumed from the surviving worktree after transcript loss. | 3/5 externally verified; two failed agents still claimed completion. | Retained as the narrow hard-loss baseline. |
| 2. Durable ledger | Added goal, requirement, steering, evidence, and closure events; later turns received unresolved and preserved constraints. | Workflow reached 5/5 with zero premature completions on the same five cases. | Kept. Claim only hard-loss recovery; one trial per arm. |
| 3. Visible grader | Copied a late integration test into the writable candidate before continuation. | Both modes could inspect or edit the supposed grader. | Removed from scoring; graders stay external and fingerprinted. |
| 4. Ambiguous fixture | A rename grader required API names absent from the written requirement. | Both outcomes were semantically plausible but unscorable. | Discarded and replaced with an explicit interface contract. |
| 5. Doubt-driven audit | Tested whether the 5/5 result proved native long-session reliability. | Baseline never received late steering; three cases tied; no native resume or compaction occurred. | Relabelled as hard-loss fault injection. |
| 6. Product-drift correction | The first standalone product became an inspector plus isolated child, not the requested continuous keeper. | No worker remained attached after one-shot grounding. | Retained recovery tools; moved the primary runtime to exact-session lifecycle hooks. |
| 7. Opt-in same-session plugin | Added exact enrollment, unenrolled no-op, `$driftctl on|status|off`, prompt reconciliation, and injection. | Installed subprocess tests proved explicit activation and same-repository isolation. | Kept. Installation alone remains inert. |
| 8. Semantic commit boundary | Luna proposes source-linked transitions; deterministic code validates shape, provenance, stale bases, and projection size. | Invalid additions, supersessions, conflicts, overflow, and replay fail before model work. | Kept one bounded repair; model output never writes state directly. |
| 9. Lifecycle recovery | Split hook receipts from provider record IDs and added `Stop`, `PreCompact`, and `SessionStart` recovery. | A real session retained `DRIFT_MARKER` across a separate resume; production-shaped compaction restored it. | Kept as process-boundary evidence, not coding-efficacy evidence. |
| 10. Conflict and goal gate | Added durable alternatives and operator-only decisions; removed automatic same-session goal mutation after a race was found. | Wrong, stale, replayed, and concurrent approvals cannot mutate native goals through Driftctl. | Kept fail-closed behavior and explicit `/goal` handoff. |
| 11. Long-session intake | Imported native compaction markers, bounded provider responses, and compressed non-user evidence to digests. | A real 13 MB/5,703-item session was processed without granting tool output steering authority. | Kept bounded intake; buffered transport and model latency remain limitations. |
| 12. Native efficacy suite | Compared equal native-history arms with external graders and order controls. | Baseline 3/4; workflow 2/4. | Frozen as a negative result; no selective rerun. |
| 13. Prospective/historical A/B | Added equal idle forks, completed-turn selection, immutable Git checkpoints, and one external verifier for both arms. | Packaged rehearsals proved lineage, prefix selection, isolation, and report caching; the prospective result was a tie. | Kept as evaluation tooling, not efficacy proof. |
| 14. macOS incident | Namespaced activation was ignored and children followed inherited parent paths. | Parent changed while candidates stayed clean; run was invalid. | Accepted exact qualified controls and blocked unsafe parent-path checkpoints before forking. |
| 15. Security gate | Targeted review found public temporary-file modes, symlink-following trajectory storage, and candidate-local verifier substitution. | Red tests reproduced each issue; private modes, symlink rejection, and external verifier pinning passed packaged checks. | Fixed release blockers; no absolute security claim. |
| 16. Real Click client A/B | Reconstructed the path-neutral Click 8.2.1 checkpoint and used only released Driftctl/Codex client commands. | Baseline passed 1/1 and the full suite. Workflow blocked a real goal conflict, then deadlocked during goal recovery and failed 0/1; reporting rejected CLI attachment. | Keep the safety refusal. Treat goal recovery, hook JSON, and activation/report compatibility as blockers. |
| 17. Submission package | Reduced judge-facing documentation and separated narrow positive evidence from negative broad evidence. | Reproduction commands, retained results, trajectories, and security scan are linked from the root. | Submit as an honest MVP; do not claim proven long-horizon efficacy. |

## Main failure mode

A structurally faithful projection does not guarantee semantic correctness. A correct refusal also has little user value if recovery cannot complete; the Click client run exposed exactly that gap.

## Hot take

The projection was not automatically the win; refusal and external evidence were. More context, reasoning, agents, or reviews did not improve the frozen native cases.
