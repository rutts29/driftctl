# Driftctl

Driftctl is an opt-in, local continuity layer for long Codex sessions. It keeps a source-linked record of the accepted goal and later steering, validates semantic updates, and injects the current intent after prompts, resume, and compaction.

> **Hackathon MVP.** The CLI and plugin pass their deterministic process tests. The hard-loss evaluation improved verified completion from **3/5 to 5/5**. We have not proved a broader same-session benefit. The latest real client case scored **baseline 1/1, workflow 0/1** and exposed goal-recovery and reporting bugs. Both results are in this repository.

## The problem

Developers and maintainers run coding-agent sessions that last for hours or days. The initial goal and later steering can get diluted after many turns, interruptions, and compaction. The agent may follow stale intent, forget a supersession, or declare completion against the wrong objective.

Driftctl records accepted intent outside the transcript. It refuses ambiguous changes instead of guessing.

## Product flow

```text
existing Codex session
  → user invokes exact `$driftctl on`
  → hook binds only that session
  → Luna proposes a source-linked semantic delta
  → deterministic validator commits or blocks
  → compact active intent is injected before work
  → resume/compaction restores the same projection
  → `$driftctl off` detaches only that session
```

Installation is inert. There is no daemon, hosted service, telemetry, credential copy, or rewrite of `AGENTS.md`, `CLAUDE.md`, user skills, permissions, or worktrees.

## Install and try

Current packaged target: Linux x86_64. Driftctl has been tested with Codex CLI 0.150.1 and 0.151.0. An existing local Codex login is required.

```bash
git clone https://github.com/rutts29/driftctl.git
cd driftctl
cargo install --path . --locked
driftctl integrate codex install
driftctl integrate codex status
```

Approve the Driftctl hook source in Codex's `/hooks` UI. Then, inside one existing session:

```text
$driftctl status
$driftctl on
From now on, end every answer with exactly DRIFT_MARKER.
```

Exit and resume that exact session, then ask a normal question without repeating the marker instruction. Finish with:

```text
$driftctl off
```

Codex may render controls as `$driftctl-codex:driftctl on|status|off`; those exact forms are supported. Near matches do nothing.

## Evidence

| Evaluation | Baseline | Workflow | What it establishes |
|---|---:|---:|---|
| Five fixed hard-loss cases | 3/5 | **5/5** | Narrow recovery improvement after steering is removed from a fresh agent's context |
| Four native-history coding cases | **3/4** | 2/4 | No measured coding-efficacy improvement |
| Click 8.2.1 real client case | **1/1** | 0/1 | Correct conflict refusal, followed by a broken goal-approval recovery path |
| Restart/compaction acceptance | n/a | pass | Accepted marker constraint restored in the same real session |

Primary metric: externally verified completion with allowed scope and no premature completion. Each coding case was run once; timing and usage are descriptive, not statistically causal.

- Results: [`evals/results/`](evals/results/)
- Representative sanitized trajectories and local visual timeline: [`evals/trajectories/`](evals/trajectories/)
- Improvement history: [`IMPROVEMENT-CHANGELOG.md`](IMPROVEMENT-CHANGELOG.md)
- Exact commands: [`REPRODUCING.md`](REPRODUCING.md)
- Judge summary: [`SUBMISSION.md`](SUBMISSION.md)

## Current limits

- Codex is the only native lifecycle adapter.
- Keeper calls consume the user's allowance and add latency.
- Model proposals can be semantically wrong; deterministic checks validate structure and provenance, not meaning.
- The real goal-change handoff can invalidate its own pending decision; this blocks the broad end-to-end claim.
- `ab report` currently requires transcript activation and does not accept an otherwise valid CLI attachment.
- Driftctl is not a security sandbox and does not make host-wide or YOLO permissions safe.
- The name collides with an existing public tool; rename before package-manager distribution.

## Evaluation next

The next evaluation will use multiple trials on organic, long-running sessions that cross real compaction and contain late supersession, conflict, and interruption. Small coding cases are weak discriminators because frontier models already solve many of them; the useful question is whether accepted intent survives long-horizon pressure. These future runs are not part of the submitted evidence.

## Project provenance

The continuity idea and supporting infrastructure existed in a broader private engineering harness before the event. During the hackathon, the workflow was extracted and adapted into this standalone Rust CLI/plugin, reproducible fixtures, installer, and public evidence package. Claims here apply only to this repository; the private harness is neither required nor submitted.

## Development checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s evals/tests -p 'test_*.py' -v
shellcheck scripts/package-release.sh scripts/install.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest release.tests.test_installer -v
```

Raw Codex records may contain private paths, prompts, and session identifiers. Keep them outside Git and publish only manually reviewed sanitized trajectories.
