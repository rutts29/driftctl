# Reproducing the current evaluation contract

This guide runs the current hard transcript-loss baseline and workflow on synthetic cases. Live runs require only an authenticated Codex CLI; raw trajectories remain local.

## Evaluation boundary

Both modes start with the same clean fixture, model configuration, tools, initial goal, and visible tests. After the initial turn, one late steering requirement is considered part of the lost session context.

- Baseline: a fresh ephemeral Codex session receives only the surviving worktree and a generic recovery prompt.
- Workflow: the same fresh-session interruption occurs, but `driftctl` reconstructs the initial goal and late steering from its ledger.

This measures recovery after a hard transcript loss. It does not compare against native Codex/Claude resume or ordinary context compaction.

## Tested environment

- Linux
- Git 2.53.0
- Rust and Cargo 1.97.1, also pinned by `rust-toolchain.toml`
- Python 3.14.4; the fixtures require Python 3.11 or newer
- `codex-cli 0.150.1`
- Model `gpt-5.6-sol`
- Reasoning effort `xhigh`

The runners inherit the Codex model and reasoning settings from the executing user's Codex configuration. Confirm those two settings before comparing results; do not publish the rest of a personal configuration file.

## Clean setup

```bash
git clone <repository-url> driftctl
cd driftctl
rustc --version
python3 --version
codex --version
cargo build --locked
```

Run the deterministic checks first:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  evals.tests.test_baseline_runner \
  evals.tests.test_native_long_session_runner \
  evals.tests.test_workflow_runner \
  evals.tests.test_score_results -v
```

## Run one paired case

The following commands keep raw provider trajectories and result JSON outside the repository:

```bash
mkdir -p /tmp/driftctl-reproduction/baseline /tmp/driftctl-reproduction/workflow

PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_baseline.py \
  --case evals/cases/05-rename-resume \
  --codex-bin codex \
  --artifacts /tmp/driftctl-reproduction/baseline \
  > /tmp/driftctl-reproduction/05-baseline.json

PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_workflow.py \
  --case evals/cases/05-rename-resume \
  --driftctl-bin "$PWD/target/debug/driftctl" \
  --codex-bin codex \
  --artifacts /tmp/driftctl-reproduction/workflow \
  > /tmp/driftctl-reproduction/05-workflow.json

python3 evals/runner/score_results.py \
  /tmp/driftctl-reproduction/05-baseline.json \
  /tmp/driftctl-reproduction/05-workflow.json
```

Expected structure, not an independently guaranteed stochastic outcome:

- Both result files identify the same case and grader SHA-256 fingerprint.
- The baseline reports `recovery_context: "worktree_only"`.
- The workflow records a blocked pre-evidence closure attempt.
- `verified_completion` depends on process completion, external graders, the exact declared mutation scope, and workflow closure—not the agent's final message.

## Run all five pairs

Run each directory under `evals/cases` once per mode, preserving every JSON output. Then pass the ten new JSON files to `score_results.py`.

The recorded valid runs took about 35 minutes sequentially and reported 5,527,378 total input-plus-output tokens across both modes. Dollar cost is not claimed because the authenticated CLI did not emit price data; use the executing account's billing or quota telemetry.

The committed first-pass files under `evals/results` predate exact mutation-scope scoring. They are retained without alteration as historical evidence and are intentionally rejected by the current fail-closed scorer. Do not combine them with new-fingerprint runs.

## Inspecting evidence safely

Raw provider JSONL may contain local paths or private user context. Keep it outside the public repository. Use the included sanitizer before publishing selected trajectories, then manually inspect the output. Do not delete or rewrite the case contracts, graders, or original result JSON when adding independent reruns; store reruns under new filenames.
