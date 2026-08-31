# Reproduction guide

## Environment

- Linux x86_64.
- Rust/Cargo 1.97.1.
- Python 3.11+.
- Git.
- Codex CLI 0.150.1 or 0.151.0 plus an existing login for live agent runs.

Live runs consume the executing user's Codex allowance. The deterministic suite requires no credentials or model calls.

## 1. Build and deterministic checks

```bash
git clone https://github.com/rutts29/driftctl.git
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

Expected: every command exits `0`. The Rust suite includes real subprocess, filesystem-mode, lock, malformed-input, plugin install/remove, fake App Server, isolation, and verifier-boundary cases.

## 2. Inspect retained results

No model call is required:

```bash
python3 -m json.tool evals/results/summary.json
```

Expected primary metric:

```text
baseline: 3/5 verified (0.60), 2 premature completions
workflow: 5/5 verified (1.00), 0 premature completions
```

This displays the immutable historical summary; it does not recompute it. The legacy arm files predate fields now required by the stricter scorer and are retained rather than rewritten. The result is limited to hard transcript-loss recovery, not native resume or ordinary compaction. Section 3 produces fresh, current-schema arms and scores them.

## 3. Rerun baseline and workflow

This uses Codex and can take roughly 35 to 45 minutes sequentially. The retained run used about 5.45 million input tokens across both modes. Much of that input was cached.

```bash
cargo build --release --locked
mkdir -p /tmp/driftctl-eval-results

for case in evals/cases/0[1-5]-*; do
  id=$(basename "$case")
  python3 evals/runner/run_baseline.py \
    --case "$case" \
    > "/tmp/driftctl-eval-results/${id}-baseline.json"
  python3 evals/runner/run_workflow.py \
    --case "$case" \
    --driftctl-bin target/release/driftctl \
    > "/tmp/driftctl-eval-results/${id}-workflow.json"
done

python3 evals/runner/score_results.py \
  /tmp/driftctl-eval-results/*.json
```

Use the same Codex binary, account, model policy, cases, and ordering for both modes. New runs may differ because the model is nondeterministic; do not overwrite retained results.

## 4. Install the plugin locally

```bash
cargo install --path . --locked
driftctl integrate codex install
driftctl integrate codex status
```

Open Codex, approve the hook source in `/hooks`, then invoke inside one disposable persisted session:

```text
$driftctl status
$driftctl on
From now on, end every answer with exactly DRIFT_MARKER.
```

Exit and resume that session. Ask a normal question without repeating the constraint. Expected: the answer still ends with `DRIFT_MARKER`; another session in the same repository remains unaffected.

Detach:

```text
$driftctl off
```

Do not use a consequential repository for this acceptance run. Goal-change recovery has a known blocker described in `evals/results/click-client-ab-20260831.json`.

## 5. Package rehearsal

```bash
export DRIFTCTL_REHEARSAL=/tmp/driftctl-reproduction
mkdir -p "$DRIFTCTL_REHEARSAL/releases/v0.5.1" "$DRIFTCTL_REHEARSAL/bin"

sh scripts/package-release.sh --out "$DRIFTCTL_REHEARSAL/releases/v0.5.1"
DRIFTCTL_BASE_URL="file://$DRIFTCTL_REHEARSAL/releases" \
  sh scripts/install.sh --version v0.5.1 \
  --bin-dir "$DRIFTCTL_REHEARSAL/bin"
"$DRIFTCTL_REHEARSAL/bin/driftctl" --help
```

Expected: the installer pins the version, verifies SHA-256, rejects unexpected archive entries, and installs one executable. Only `x86_64-unknown-linux-gnu` is supported.

## Evidence map

- Five-case result: `evals/results/summary.json`.
- Native negative result: `evals/results/native-suite-20260830/summary.json`.
- Latest client failure: `evals/results/click-client-ab-20260831.json`.
- A/B pipeline rehearsals: `evals/results/prospective-ab-pipeline-20260830.json` and `historical-ab-pipeline-20260831.json`.
- Sanitized trajectories: `evals/trajectories/`.

## Privacy and cleanup

- Do not publish `$CODEX_HOME`, XDG Driftctl state, raw trajectories, session IDs, or authentication files.
- Use only disposable repositories for live reproduction.
- `driftctl integrate codex remove` removes the integration; `$driftctl off` detaches only the invoking session.
- Manually inspect every sanitized trajectory before publication.
