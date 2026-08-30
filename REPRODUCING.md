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

Start with an existing persisted Codex session associated with a disposable Git repository. Copy its exact session UUID.

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

Approval must execute `goal/get → goal/clear → goal/set → goal/get` against the exact session. Edit and reject cannot mutate the native goal. Stale, wrong-session, and replayed approval must fail without mutation.

Detach:

```bash
driftctl detach codex --session <exact-session-id> --json
```

A later hook for that session must emit nothing and create no state.

## Package rehearsal

Use a disposable path; do not overwrite the normal installed binary:

```bash
export DRIFTCTL_REHEARSAL=/tmp/driftctl-reproduction
mkdir -p "$DRIFTCTL_REHEARSAL/releases/v0.2.0" \
  "$DRIFTCTL_REHEARSAL/bin"

sh scripts/package-release.sh \
  --out "$DRIFTCTL_REHEARSAL/releases/v0.2.0"

DRIFTCTL_BASE_URL="file://$DRIFTCTL_REHEARSAL/releases" \
  sh scripts/install.sh --version v0.2.0 \
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
