# driftctl

`driftctl` is a small, harness-independent continuity layer for long-running coding-agent tasks. It preserves the goal, requirements, later steering, evidence, and closure state in an append-only JSONL ledger so a fresh agent session can resume without reconstructing intent from memory.

It is aimed at developers and maintainers who delegate multi-step changes to coding agents and need to recover after a transcript or session is lost. It does not replace `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, or the harness itself.

## What it ships

- A standalone Rust CLI with no service or database.
- An append-only `.driftctl/ledger.jsonl` stored inside the target repository.
- A generic `resume --json` contract that any harness wrapper can consume.
- One working convenience adapter for Codex CLI.
- Five synthetic evaluation cases, external fingerprinted graders, baseline/workflow runners, recorded results, and sanitized trajectories.

## Hackathon target

- Current evidence: hard transcript-loss pilot.
- Target problem: intent drift inside an intact long-running session.
- Target intervention: native history plus a bounded, source-linked active-intent projection.
- Target comparison: identical native child forks and workspace checkpoints, with and without the projection.
- Target UX: inspect an existing local session, compare disposable children, adopt only an explicitly selected child.
- Target safety boundary: Driftctl writes only local state and isolated children; host-wide provider YOLO permission remains outside its containment guarantee.
- Product contract: [`SPEC.md`](SPEC.md).
- Runtime design: [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Dependency-gated implementation: [`tasks/plan.md`](tasks/plan.md).

This is a continuity adapter, not a security sandbox. The Codex adapter uses Codex's own documented `workspace-write` sandbox and verifies that root `AGENTS.md`, root `CLAUDE.md`, and the drift ledger remain unchanged during an agent run.

## Quick start

Prerequisites are Git and Rust 1.97.1. Python 3.11 or newer is needed only for the included evaluation cases.

```bash
git clone <repository-url> driftctl
cd driftctl
cargo build --release --locked
cargo install --path . --locked
```

Inside the repository where an agent will work:

```bash
driftctl start \
  --goal "Add retry support" \
  --requirement "Retry transient failures once"

driftctl steer \
  --requirement "Never retry 401 or 403 responses"

driftctl resume
```

`resume` returns the complete durable task, including satisfied constraints that later sessions must preserve:

```text
goal: Add retry support
unresolved: R1, R2
requirements:
- R1 [unresolved]: Retry transient failures once
- R2 [unresolved]: Never retry 401 or 403 responses
closed: false
```

After an external check establishes evidence:

```bash
driftctl satisfy --id R1 --evidence "retry unit tests passed"
driftctl satisfy --id R2 --evidence "authorization integration tests passed"
driftctl close
```

`close` exits with code `2` while any requirement is unresolved and prints `verified` only after evidence exists for every requirement.

## Harness integration

The stable integration boundary is:

```bash
driftctl resume --json
```

Its existing `goal`, `unresolved`, and `closed` fields are joined by an additive `requirements` array:

```json
{
  "goal": "Add retry support",
  "unresolved": ["R2"],
  "requirements": [
    {
      "id": "R1",
      "text": "Retry transient failures once",
      "satisfied": true,
      "evidence": "retry unit tests passed"
    },
    {
      "id": "R2",
      "text": "Never retry 401 or 403 responses",
      "satisfied": false,
      "evidence": null
    }
  ],
  "closed": false
}
```

A Claude Code wrapper, editor extension, shell integration, or another harness can read this JSON and attach it as task context when starting a fresh session. The user's existing harness instructions remain authoritative and unchanged. This repository currently includes one automated process adapter:

```bash
driftctl run codex
```

It launches `codex exec --json --ephemeral --sandbox workspace-write` with the durable goal and requirements, then stores the provider trajectory under `.driftctl/trajectories/`. Other harnesses use the JSON contract until a dedicated adapter is added; portability beyond Codex is a tested interface property, not a claim that every provider adapter already ships.

## First-pass evaluation

The primary metric is verified completion: the agent process must finish, every case-owned external grader must pass, and the workflow must close without unresolved requirements.

| Mode | Verified completion | Premature completion | Mean time | Total tokens |
|---|---:|---:|---:|---:|
| Worktree-only recovery baseline | 3/5 (60%) | 2 | 200.850 s | 2,589,433 |
| `driftctl` workflow | 5/5 (100%) | 0 | 221.897 s | 2,937,945 |

The workflow improved verified completion by 40 percentage points in this first pass, while mean runtime increased 10.5% and total tokens increased 13.5%. Three cases were quality ties; the two separating cases were overlapping-page deduplication and a task-to-work-item API rename.

This is deliberately a narrow result. The baseline models a hard transcript loss: a fresh Codex session gets the existing worktree but not the lost task record or late steering. It does not represent native Codex/Claude session resume or normal context compaction. Each case/mode was run once with `codex-cli 0.150.1`, `gpt-5.6-sol`, and `xhigh` reasoning, so cost and timing are descriptive rather than causal. Independent review and reruns are expected.

Inspect the evidence directly:

- Case contracts and grader tests: [`evals/cases`](evals/cases)
- Baseline and workflow runners: [`evals/runner`](evals/runner)
- Sanitized results: [`evals/results`](evals/results)
- Evaluator tests: [`evals/tests`](evals/tests)
- Sanitized representative trajectories: [`evals/trajectories`](evals/trajectories)

Run the deterministic project checks:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  evals.tests.test_baseline_runner \
  evals.tests.test_workflow_runner \
  evals.tests.test_score_results -v
```

Score the recorded five-case results:

```bash
python3 evals/runner/score_results.py evals/results/0[1-5]-*.json
```

Raw live trajectories are intentionally not committed because provider output can contain local paths or private context. The committed copies are generated specifically for the evaluation and sanitized before publication.
