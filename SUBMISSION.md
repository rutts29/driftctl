# Hackathon Submission Sheet

## Working title

- Internal name: `driftctl`.
- Public name: pending; the internal name collides with an existing CLI and Homebrew formula.
- Deliverable: standalone local CLI plus strict neutral-session bundle schema.

## Problem

- User: developers and maintainers continuing long coding-agent sessions.
- Bottleneck: late steering, supersession, compaction, and interruption make the active goal hard to recover; agents can claim completion without causal evidence.
- Desired outcome: an isolated candidate that preserves active intent, source state, native goal, and existing behavior, or an explicit operator blocker.

## Workflow

```text
existing local session
  -> read-only import
  -> bounded source-linked projection
  -> isolated child continuation
  -> requirement checks + regression + integration + protected scope + review
  -> verified completion or explicit blocker
```

- Codex: native inspect, fork, goal migration/read-back, compare, and continuation.
- Other harnesses: export the documented schema-v1 neutral bundle; Driftctl validates and projects it locally. Native continuation is not claimed.
- Existing `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, credentials, and source session remain unchanged.
- No hosted service or telemetry; provider calls use the operator's existing authentication and allowance.

## Evaluation result

| Outcome | Baseline | Driftctl | Plain summary |
|---|---:|---:|---:|
| Externally verified coding cases | 3/4 | 2/4 | 1/1 |
| Measured coding improvement | none | none | not a powered comparison |

- Four constructed long-session cases reached real Luna Max coding continuations.
- Case 05 stopped before coding on an unresolved, source-linked conflict; it is a safety outcome, not a completion failure.
- All nine coding candidates passed fixed tests and exact scope; one blind review blocked three candidates with `Required` defects.
- Published arms ran baseline-first. This possible order effect is retained as a limitation. The reproducible runner now alternates baseline-first and workflow-first and records the order; published outcomes were not rerun.
- The scored sessions were deliberately constructed benchmarks. A separate 13,191,721-byte organic session proved import/compaction transport with a deterministic proposal. A second immutable organic snapshot with 42 user records ran the real default Luna Max resolver: retained failures led to one validated, safely blocked projection; a later product attempt still failed one repair on a conflicted-item transition. Prompt v4 is process-tested but awaits the operator's manual trial. No organic run proves coding efficacy because no eligible continuation completed.

Evidence: [`evals/results/native-suite-20260830/summary.json`](evals/results/native-suite-20260830/summary.json), [`evals/results/native-suite-20260830`](evals/results/native-suite-20260830), and [`IMPROVEMENT-CHANGELOG.md`](IMPROVEMENT-CHANGELOG.md).

## Main contribution

- Highest-impact retained change: candidate-bound independent closure. Fixed tests and scope accepted 9/9 candidates; the one-pass review found `Required` defects in 3/9 and prevented verified-completion claims.
- Removed experiment: a visible injected integration grader. Agents could inspect or edit it, so its passing result did not test continuity; the grader was moved outside writable candidates and the original result was excluded from scoring.
- Main failure mode: a structurally faithful projection does not guarantee better code when native history already preserves enough intent.
- Hot take: the measured win was refusal, not the projection. Reliability came from source-linked operator blocks and external causal evidence, not additional context, reasoning, agents, or review loops.

## Five-minute video

| Time | Show | Evidence |
|---|---|---|
| 0:00–0:35 | Long-session user and failure mode | One constructed task with late steering and interruption-shaped history |
| 0:35–1:05 | Baseline and completion predicate | Same checkpoint, prompt, model, tools, sandbox, and fixed graders |
| 1:05–1:50 | Installed CLI inspection | `driftctl inspect codex --last`; source-linked goal/frontier/conflict output |
| 1:50–2:30 | Isolation and conflict behavior | `driftctl compare codex --last`; case-05 no-TTY block before child creation |
| 2:30–3:20 | Causal closure | `continue`, one requirement-bound `verify`, then four candidate-bound gates; show blockers before completion |
| 3:20–4:05 | Complete frozen result | Baseline 3/4, Driftctl 2/4, plain summary 1/1, case 05 separate, no significance claim |
| 4:05–4:35 | Changelog | Independent review blocked 3/9; visible-grader experiment removed |
| 4:35–5:00 | Limits and hot take | Constructed scored sessions; Codex-only native adapter; neutral-bundle seam; YOLO outside containment |

## Publication boundaries

- Standalone extraction: the broader private engineering harness, provisioning, credentials, personal configuration, and unrelated security infrastructure are not included and are not required.
- Raw provider trajectories, session locators, workspaces, and reviewer rationale remain private; only sanitized representative trajectories ship.
- Workspace isolation does not contain inherited host-wide or YOLO permissions.
- Supported artifact: checksum-installed `x86_64-unknown-linux-gnu` binary.
- Deferred: Homebrew, macOS, Linux ARM, npm, MCP, provider plugins, automatic adoption, and native non-Codex adapters.

Reproduction: [`REPRODUCING.md`](REPRODUCING.md). Claim index: [`README.md`](README.md#claim-index).
