# Reproduction Guide

## Recorded environment

- Linux x86_64.
- Codex CLI 0.150.1 with stable lifecycle hooks.
- Rust/Cargo 1.97.1.
- Python 3.14.4; evaluator requires 3.11+.
- Git 2.53.0.
- Default keeper: `gpt-5.6-luna`, reasoning `max`.

Live tests use the executing user's Codex authentication and allowance. Semantic calls can take seconds to several minutes. Driftctl has no service or telemetry.

## Deterministic checks

```bash
git clone <repository-url> driftctl
cd driftctl

cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s evals/tests -p 'test_*.py' -v

shellcheck scripts/package-release.sh scripts/install.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  release.tests.test_installer -v
```

The hook process suite exercises real CLI subprocess, stdin/stdout, filesystem modes, locks, fake App Server protocol, and plugin install/remove boundaries:

```bash
cargo test --locked --test hook_process -- --nocapture
```

## Build and integrate

```bash
cargo install --path . --locked
driftctl integrate codex install
driftctl integrate codex status
```

Open Codex and approve the Driftctl hook source in `/hooks`. Do not use a hook-trust bypass for ordinary operation.

Expected integration status:

```text
plugin: installed
hooks: enabled
trust: approve Driftctl hooks with `/hooks` on first use
```

## Same-session acceptance

Start with an existing persisted Codex session associated with a disposable Git repository. In that session invoke:

```text
$driftctl status
$driftctl on
```

Expected:

- Status initially reports off without creating state.
- On bootstraps and injects the existing session's active projection.
- No session ID or Codex-home path is required.
- A second session in the same repository remains silent.

The external CLI path remains available for diagnostics:

From that repository:

```bash
driftctl attach codex --session <exact-session-id> --json
driftctl status codex --session <exact-session-id> --json
```

Expected:

- `status: attached`.
- Redacted session identifier in public output.
- Private enrollment and run state under `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`.

In the attached Codex session, add a durable, observable constraint:

```text
From now on, every answer in this session must end with exactly DRIFT_MARKER.
```

Expected:

- Driftctl reconciles before the model turn.
- A valid proposal advances the projection.
- The reply ends with `DRIFT_MARKER`.

Exit Codex. Resume the same exact session in a new process and ask a normal question without repeating the marker instruction.

Expected:

- `SessionStart(resume)` injects the stored projection.
- The reply still ends with `DRIFT_MARKER`.
- `Stop` binds the hook prompt to Codex's persisted provider record without a duplicate semantic call, even when the two IDs differ.

Read the current sanitized projection:

```bash
driftctl status codex --session <exact-session-id> --json
driftctl bundle --run <run-id> --json
```

The bundle must contain the accepted goal and marker constraint with no unresolved conflict.

## Compaction boundary

Normal Codex compaction should trigger the installed `PreCompact` and later `SessionStart(compact)` hooks. For a deterministic production-shaped check, pass the same event payloads through the installed entrypoint and exact attached session:

```text
PreCompact(auto) → no stdout after durable reconciliation
SessionStart(compact) → additionalContext containing the same projection
```

The process case is `k08_k12_user_prompt_is_folded_before_injection_and_duplicate_is_idempotent`. The retained real boundary restored the same goal and marker with zero conflicts.

## Conflict and goal decisions

When a hook blocks, get the run ID and inspect alternatives:

```bash
driftctl status codex --session <exact-session-id> --json
driftctl bundle --run <run-id> --json
```

Resolve one conflict:

```bash
driftctl resolve codex \
  --session <exact-session-id> \
  --conflict <conflict-id> \
  --alternative <alternative-id> --json
```

Goal proposal choices:

```bash
driftctl resolve codex --session <exact-session-id> --reject-goal --json
driftctl resolve codex --session <exact-session-id> --edit-goal '<replacement>' --json
driftctl resolve codex --session <exact-session-id> --approve-goal --json
```

If approval reports a native-goal mismatch, it must exit `2` without a mutating goal RPC. In the exact attached Codex session, run:

```text
/goal clear
/goal <replacement>
```

Rerun `--approve-goal`. It must commit only after read-only exact goal and source-head verification. Edit, reject, concurrent change, stale, wrong-session, and replayed approval must leave the native goal untouched.

Detach:

```bash
driftctl detach codex --session <exact-session-id> --json
```

A later hook for that session must emit nothing and create no state.

## Prospective paired A/B

Use a clean disposable repository and a detached Codex session persisted at the intended midpoint:

```bash
driftctl ab prepare codex --session <midpoint-session-id> --json
```

Expected:

- `experiment_kind: prospective_paired` and `status: ready`.
- Distinct baseline/workflow session IDs and working directories.
- Equal starting candidate digest, inherited goal, and worker policy.
- No started turn, enrollment, or source-checkout mutation.

Resume the baseline from `baseline.cwd` and continue normally. Resume the workflow from `workflow.cwd`, invoke `$driftctl on`, then provide the same continuation task. Do not enroll the baseline or source session.

Create one executable verifier outside both candidate workspaces that exits zero only when the user-visible task is complete. Run it before detaching the workflow. Driftctl resolves a relative program from this invocation directory, canonicalizes it before changing into either candidate, and pins its content digest:

```bash
cd <source-repository>
driftctl ab report --run <run-id> --json -- <verifier> [args...]
```

Expected:

- Baseline `detached`; workflow `attached_exact`; source unchanged.
- The same verifier command executes once in each candidate.
- Candidate-local verifier programs or inputs are rejected before execution.
- Per-arm exit status, candidate/verifier digests, timing, post-checkpoint records/prompts, and workflow keeper overhead.
- A repeated identical report returns `cached: true`; a different verifier is rejected.

Then invoke `$driftctl off` in the workflow arm. Use verified completion as the primary outcome. Treat turns, corrections, elapsed time, and keeper usage as secondary. If prompts or operator interventions differed, label the run a pipeline rehearsal rather than a controlled efficacy result.

The retained sanitized rehearsal is `evals/results/prospective-ab-pipeline-20260830.json`. It proves the fork/enrollment/report lifecycle and records a tie; it does not supersede the negative frozen efficacy evaluation.

### Historical completed-turn checkpoint

Choose a completed Codex turn and the Git commit representing its repository state:

```bash
driftctl ab prepare codex \
  --session <source-session-id> \
  --through-turn <completed-turn-id> \
  --source-ref <git-commit> \
  --json
```

Expected:

- `checkpoint.kind: historical_turn`.
- `checkpoint.through_turn_id` equals the requested turn.
- `checkpoint.source_commit` is the resolved immutable commit.
- Both fork requests use the same `lastTurnId`; later source turns are absent.
- Both candidate workspaces contain the selected commit, not current or uncommitted files.
- An in-progress selected turn exits `2` before semantic work or mutation and names the preceding completed turn when available.

Codex does not expose turn-versioned native-goal history. Historical output therefore labels the inherited current goal as `native_goal_basis: current_at_prepare`. Driftctl does not infer a Git checkpoint from conversation text and cannot recover uncommitted historical files.

The sanitized packaged real-provider rehearsal is `evals/results/historical-ab-pipeline-20260831.json`. It proves checkpoint selection, prefix exclusion, commit binding, source preservation, and idle forks; it does not measure coding efficacy.

## Package rehearsal

Use a disposable path; do not overwrite the normal installed binary:

```bash
export DRIFTCTL_REHEARSAL=/tmp/driftctl-reproduction
mkdir -p "$DRIFTCTL_REHEARSAL/releases/v0.5.0" \
  "$DRIFTCTL_REHEARSAL/bin"

sh scripts/package-release.sh \
  --out "$DRIFTCTL_REHEARSAL/releases/v0.5.0"

DRIFTCTL_BASE_URL="file://$DRIFTCTL_REHEARSAL/releases" \
  sh scripts/install.sh --version v0.5.0 \
    --bin-dir "$DRIFTCTL_REHEARSAL/bin"

"$DRIFTCTL_REHEARSAL/bin/driftctl" --help
```

The installer pins the version, verifies SHA-256, rejects unexpected archive entries, invokes the installed entrypoint, and supports only `x86_64-unknown-linux-gnu`. GitHub download works after the private release is published.

## Retained evaluation evidence

- Frozen coding comparison: baseline 3/4; projected workflow 2/4; no measured efficacy improvement.
- Safety case: unresolved ambiguity blocked before coding.
- Archived hard-loss cases are historical fault-injection evidence and are not the same-session baseline.

Re-score immutable native results without overwriting them:

```bash
python3 evals/runner/score_results.py \
  evals/results/native-suite-20260830/*-native-*.json
```

Do not selectively rerun or overwrite retained cases. Store new same-session baseline/workflow runs under new result paths.

## Privacy

- Keep XDG state, raw trajectories, provider session IDs, and artifacts private.
- Raw Codex records can include local paths, prompts, tools, and private context.
- Sanitize selected trajectories with `evals/runner/sanitize_trajectory.py`, then inspect them manually before publication.
- Never publish authentication files, credentials, the broader private engineering harness, or unrelated observability data.
