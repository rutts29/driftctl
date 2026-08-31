# Hackathon submission

## One-minute summary

| Question | Answer |
|---|---|
| Who has the problem? | Developers and maintainers continuing long coding-agent sessions. |
| What goes wrong? | Goals and steering get diluted after many turns, interruption, or compaction. |
| What did I build? | A local Rust CLI and explicit opt-in Codex lifecycle plugin. |
| How does it work? | A model proposes semantic updates. Deterministic code validates them, stores source-linked history, and asks the operator to resolve ambiguity. |
| What improved? | Hard-loss fixtures improved from **3/5 to 5/5 verified completion**. |
| What did not? | Native-history evaluation was 2/4 versus baseline 3/4. The latest real-client case was 0/1 versus baseline 1/1. |

## What each part does

| Component | Responsibility |
|---|---|
| Luna Max keeper | Propose one semantic change from the active projection plus new user input |
| Deterministic validator | Enforce source coverage, legal transitions, stale-state checks, and size limits |
| Lifecycle hooks | Reconcile and inject before work; persist across stop, resume, and compaction |
| Operator gate | Resolve conflicts and native-goal changes; the keeper cannot approve itself |
| External verifier | Decide completion independently of the coding agent's claim |

## Measured results

| Evidence | Result | Scope |
|---|---:|---|
| Fixed five-case hard-loss suite | baseline 3/5; workflow **5/5** | One trial per arm; fresh agent loses late steering |
| Native-history suite | baseline **3/4**; workflow 2/4 | Negative efficacy result |
| Real Click client A/B | baseline **1/1**; workflow 0/1 | Reconstructed path-neutral post-investigation checkpoint |
| Same-session marker acceptance | pass | Real resume plus production-shaped compaction lifecycle |
| Unresolved conflict gate | pass | Continuation blocked before coding |

The Click workflow correctly refused a real inherited-goal conflict. It then failed to recover: the required `/goal` handoff changed the session and invalidated the pending decision. The same run exposed invalid `SessionStart` hook output and an activation/report compatibility gap. These are open blockers, not hidden failures.

## What existed before the event

- Broader private engineering harness and continuity concept.

## What this event added

- Standalone Rust implementation and Codex plugin.
- Explicit per-session activation and isolation.
- Reproducible baseline/workflow runners and fixed cases.
- Prospective and historical A/B tooling.
- Checksum installer, package tests, sanitized evidence, and release-focused security fixes.

The private harness is not required to build, run, or score this repository.

## Required package map

| Deliverable | Location |
|---|---|
| Solution code and agent instructions | `src/`, `plugins/`, `AGENTS.md` |
| Improvement changelog | `IMPROVEMENT-CHANGELOG.md` |
| Reproduction guide | `REPRODUCING.md` |
| Evaluation fixtures and results | `evals/cases/`, `evals/results/` |
| Representative trajectories and visual timeline | `evals/trajectories/` |
| Video outline | below; recording/link must be supplied separately |

## Agent-use disclosure

- OpenAI Codex was used for implementation, testing, orchestration, and documentation.
- GPT-5.6 Luna Max was used as the runtime semantic keeper and in evaluation runs.
- GPT-5.6 Sol-class reasoning was used for high-reasoning implementation work.
- Daybreak Blue performed targeted release-blocker security review.
- Committed trajectories are sanitized representatives; raw development/provider traces remain private because they include local paths and unrelated session context.

## Security check

- The current tree and full Git history contain no credential-shaped tokens, private keys, authentication files, or tracked symlinks.
- References to `.codex/auth.json` are test fixtures that verify protected-file isolation. They contain no real credentials.
- The release archive contains one executable. Checksum verification and a clean-directory installation passed.
- The binary string scan found no GitHub, OpenAI, AWS, or private-key patterns.
- Exact dependency versions are locked. This environment did not have `cargo-audit`, so this submission makes no dependency-advisory claim.

## Five-minute video outline

| Time | Show |
|---|---|
| 0:00 to 0:35 | User problem and direct baseline |
| 0:35 to 1:15 | Local inert install and explicit `$driftctl on` |
| 1:15 to 2:10 | Steering folded, validated, and injected |
| 2:10 to 2:50 | Resume/compaction retaining an observable constraint |
| 2:50 to 3:35 | Conflict refusal and operator boundary |
| 3:35 to 4:30 | Full result table, including negative native and Click results |
| 4:30 to 5:00 | Removed experiment, main failure mode, and hot take |

## Follow-up evaluation

Planned work is multiple trials on organic long sessions that cross real compaction and include superseding and conflicting steering. Small tasks often saturate frontier coding models, so they do not reliably measure the long-horizon failure this tool targets. Future results will be additive; submitted failures will not be overwritten.

## Main failure mode

A structurally valid projection can still be semantically wrong, and a correct refusal is not useful if the operator cannot recover. The current native-goal recovery path demonstrates that gap.

## Hot take

More context, agents, reasoning, or reviews do not automatically prevent drift. Reliability comes from preserving effective intent, separating proposal from commit authority, and requiring causal evidence before completion.
