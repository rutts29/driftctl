# Hackathon Submission

## Project

- Internal name: Driftctl.
- Deliverable: local Rust CLI plus opt-in Codex lifecycle plugin.
- Public rename required before broad package-manager publication because `driftctl` already exists.

## User and problem

- User: developers and maintainers continuing long coding-agent sessions.
- Bottleneck: the initial goal and later steering become diluted across long histories, interruption, and compaction.
- Failure: the agent follows stale instructions, forgets a supersession, or claims progress against the wrong objective.
- Desired result: the same ongoing session receives a compact accepted goal and effective steering before each turn and after resume or compaction.

## Product flow

```text
existing Codex session
  → operator invokes exact `$driftctl on` inside the session
  → lifecycle hook receives prompt
  → reconcile missed persisted records
  → Luna proposes source-linked semantic delta
  → deterministic validator
      → valid: durable commit + inject into same session
      → invalid/conflicting: block before model
  → Stop binds hook prompt to persisted provider record
  → resume/compact restores and injects projection
  → operator detaches exact session
```

## Purposeful agent choices

| Choice | Purpose |
|---|---|
| Luna Max keeper | Low-cost semantic proposal from bounded projection plus one prompt delta |
| Deterministic validator | Model cannot write ledger state or approve its own goal change |
| Source-linked immutable history | Superseded and rejected steering remains auditable |
| Bounded active projection | The session receives current intent, not an ever-growing transcript copy |
| Exact enrollment | Other sessions, including same-repository sessions, remain no-ops |
| Operator conflict/goal approval | Ambiguity is explicit; native same-session goal changes stay inside the operator's Codex session |
| Lifecycle hooks | Continuity follows the same session across prompt, stop, resume, and compaction |

## Current evidence

| Evidence | Result |
|---|---:|
| Hook process cases | 9/9 pass |
| Exact real-session attach and in-session activation process boundary | pass |
| Steering committed before same-session model turn | pass |
| Separate-process resume retained the constraint without repetition | pass |
| Production-shaped `PreCompact → SessionStart(compact)` recovery | pass |
| Unenrolled/same-repository control receives context | no |
| Wrong/stale/replayed goal approval mutates native goal | no |
| Driftctl same-session goal clear/set RPCs | none |

Real acceptance sequence:

1. Attached an existing persisted Codex session.
2. Added: every answer must end with `DRIFT_MARKER`.
3. Ledger advanced and the same turn returned `Understood. DRIFT_MARKER`.
4. Resumed from a separate Codex process without repeating the constraint.
5. Response restated the active intent and ended with `DRIFT_MARKER`.
6. `PreCompact` followed by `SessionStart(compact)` restored the same goal and marker with zero conflicts.

This proves the shipped workflow boundary, not statistical coding improvement.

## Baseline and evaluation honesty

- Fair final evaluation target: same model/tools and five fixed long-session tasks, comparing ordinary resume against attached Driftctl resume.
- Primary metric: verified retention of every effective requirement and supersession after interruption/compaction.
- Secondary: false completion, operator blocks, semantic calls, elapsed time, and usage.
- Retained earlier coding suite: baseline 3/4 versus projected workflow 2/4; no efficacy improvement.
- Do not combine the earlier isolated-child result with the new same-session claim.

## Improvement path

| Failure observed | Change retained |
|---|---|
| Built an inspector/fork instead of a continuous keeper | Replaced primary runtime with exact-session lifecycle hooks |
| Luna treated diagnostic/style steering as a goal replacement | Goal change now requires explicit overall-goal replacement; operator approval remains mandatory |
| Repair repeated invalid add/conflict shapes | Repair receives exact validator code and legal operation shapes |
| Hook turn ID differed from persisted provider message ID | One-to-one content-digest receipt binding within the exact enrollment |
| Untrusted automation silently skipped hooks | Integration tells the operator to review and trust Driftctl hooks |

Full retained history: [IMPROVEMENT-CHANGELOG.md](IMPROVEMENT-CHANGELOG.md).

## Hot take

More context, reasoning, agents, or reviews do not automatically prevent drift. Reliability comes from preserving effective intent, separating proposals from commit authority, and refusing continuation when the system cannot prove which instruction is active.

## Five-minute video

| Time | Show |
|---|---|
| 0:00–0:35 | Long-session drift and ordinary-resume baseline |
| 0:35–1:05 | Local install remains inert; exact in-session activation |
| 1:05–2:05 | New steering folded and injected into the same session |
| 2:05–2:50 | Exit and resume; constraint survives without repetition |
| 2:50–3:30 | Conflict or goal-change block and explicit operator decision |
| 3:30–4:10 | Compact/restart recovery and strict no-op control session |
| 4:10–4:40 | Complete measured results, including failures and usage |
| 4:40–5:00 | Changelog, limits, and hot take |

## Boundaries

- Local only; no daemon, hosted service, telemetry, or credential copy.
- Existing `AGENTS.md`, `CLAUDE.md`, skills, permissions, hooks, config, authentication, and worktree remain user-owned.
- Codex-only native adapter in the MVP.
- Raw trajectories and local state stay private; publish only sanitized reviewed examples.
- Driftctl is not a security sandbox and does not contain host-wide or YOLO permissions.
- Private repository/release until the operator makes them public.

Reproduction: [REPRODUCING.md](REPRODUCING.md).
