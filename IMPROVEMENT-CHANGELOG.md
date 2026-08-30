# Improvement changelog

This changelog records the experiments that shaped the standalone hackathon implementation. Discarded experiments remain part of the story rather than being counted as evidence.

| Stage | What changed and why | Evidence | Decision |
|---|---|---|---|
| Baseline | A fresh general-purpose Codex agent resumed from the surviving worktree after a hard transcript loss. | Final baseline: 3/5 protected cases verified; two failed agents still emitted completed turns. | Kept as the narrowly labelled worktree-only recovery baseline. |
| Ledger core | Added append-only goal, requirement, steering, evidence, and closure events. | Reopen tests preserve steering and closure exits `2` while requirements remain unresolved. | Kept. |
| Codex adapter | Reconstructed the goal/frontier for a fresh ephemeral Codex process and retained raw JSONL trajectories. | Real smoke run and adapter tests completed without changing `AGENTS.md`, `CLAUDE.md`, or the ledger. | Kept as the first convenience adapter; generic integration remains the JSON contract. |
| Visible injected grader experiment | Initially copied the late integration test into the agent's writable workspace before the second turn. | Both modes passed; agents could read and even edit the supposed grader, so the result did not test continuity. | Removed from scoring. Graders now remain external and are fingerprinted before and after runs. |
| Preserved constraints | Included satisfied requirements in later agent context, not only the unresolved frontier. | Adapter test proves a satisfied requirement remains present as a constraint to preserve. | Kept. |
| Ambiguous rename experiment | The first rename grader silently expected payload methods that the written requirement did not name. | The workflow recovered the intent but implemented a different reasonable public API; both grader outcomes were unusable. | Discarded. Made `submit_payload` and `resume_payload` explicit, then reran both modes on a new fingerprint. |
| Final five-case pass | Combined durable recovery, external graders, evidence-gated closure, and deterministic scoring. | Workflow 5/5 versus baseline 3/5; premature completion 0 versus 2. Mean time increased 10.5% and total tokens increased 13.5%. | Kept as first-pass evidence, with one trial per case and the hard-loss scope stated prominently. |
| Doubt-driven pilot review | Tested whether the five-case comparison supports a long-session reliability claim. | The baseline never received late steering; three cases tied; two favored the workflow; one run per arm; fixtures were small. | Relabelled as hard-loss fault-injection evidence; no native-resume, long-horizon, or statistical-significance claim. |
| Long-session pivot | Changed the target from replacing lost history to re-grounding an intact native session with a bounded active-intent projection. | Codex CLI exposes native session forking; critical review requires identical checkpoint branches for behavioral comparison. | Use native history in both arms; retain immutable history; compact only the active prompt projection; treat retrospective inspection as fidelity evidence only. |
| Summary reproducibility check | Compared fresh scorer output with the published summary before committing the pilot package. | `primary_metric` and `by_mode` matched; exact JSON differed because the summary adds design and caveat metadata and omits per-case rows. | Kept the enriched report and documented the reproducible metric boundary instead of claiming byte-identical output. |
| Codex session intake | Added a read-only App Server importer for `inspect codex`; it selects only the canonical current-directory session, extracts explicit user text only, and emits a redacted blocking report until projection exists. | Five subprocess protocol tests cover selection, pagination, malformed/truncated responses, redaction, and source-worktree non-mutation. A fresh disposable read-only Codex session was initially not discoverable immediately after `codex exec`; after App Server materialization, the shipped CLI imported it with exit `2` and the source worktree unchanged. | Kept. The command fails closed when no session is yet visible; the reproduction must wait for the provider session to materialize rather than claim immediate availability. |

## Main failure mode

Durable state helps only when important intent is recorded before the session disappears. It cannot recover an instruction that never reached the ledger, and it does not guarantee better reasoning: three baselines inferred the lost constraints without help.

## Hot take

More reasoning and more reviewers do not automatically make coding agents reliable. In these runs, reliability came from preserving the complete task across a real loss boundary, grading outside the agent-writable workspace, and refusing verified closure until causal evidence existed. The ledger improved recovery, but it also added measurable overhead; both facts belong in the result.
