# Reproducing current checks and native evidence

Live commands require an authenticated Codex CLI and consume the executing user's Codex usage allowance. Driftctl has no service or telemetry. Keep raw provider JSONL, session locators, and candidate artifacts private: they can contain local paths or user context.

## Recorded environment and scope

Recorded versions are Git 2.53.0, Rust/Cargo 1.97.1, Python 3.14.4 (Python 3.11+ required), and `codex-cli 0.150.1`. The native runner's recorded worker policy is `gpt-5.6-luna` with `max` reasoning. The user's active Codex configuration and account determine actual availability, timing, and usage.

The current local product flow is Codex-native: `inspect`, `bundle`, `compare`, then `continue`. State for that flow is private XDG state at `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`, not a repository ledger. `bundle` emits a versioned sanitized projection/blocker handoff; no non-Codex native adapter is claimed. Source session, source workspace, parent native goal, and existing harness instructions/configuration remain unchanged. Candidate workspaces are isolated, but inherited host-wide/YOLO permission is outside the containment guarantee.

Codex `thread/read` is bounded at 64 MiB. The importer validates documented item shapes, keeps ordered record IDs/roles/digests in private state, and sends only explicit user text plus coalesced digest evidence to the semantic resolver. `contextCompaction` is counted as a boundary, not accepted as user intent. Appending only assistant/tool evidence updates provenance with zero semantic calls.

Unsupported harnesses can provide the schema-v1 neutral session document specified in [`SPEC.md`](SPEC.md):

```bash
driftctl inspect bundle --file /path/to/session.json --json
# equivalent streaming boundary:
harness-export-command | driftctl inspect bundle --stdin --json
```

The source `repository_digest` is SHA-256 of the canonical repository path, every record carries its constructor-defined digest, and `provider: "bundle"` distinguishes this intake. Invalid schema, digest, repository identity, role, or content is rejected before model use. The same immutable bundle is cached; an updated snapshot must use a new `session_ref` in this release.

If `inspect --json` reports a semantic conflict, unattended `continue` exits `2` before creating a child. Resolve it with the exact IDs returned by inspection:

```bash
driftctl continue codex --last \
  --resolve-conflict <conflict-id> <alternative-id> --json
```

An invalid conflict or alternative ID exits `1` without forking. A valid choice is source-linked in private state and continuation proceeds with the selected alternative.

When native child goal mutation is missing or only partially confirmed, `continue --json` returns `manual_goal_handoff_required` and exits `2`. Its `resume.argv`, `resume.cwd`, and `slash_commands` fields identify the isolated child and exact manual sequence. No child turn starts; a new operator approval and goal verification are required before manual continuation.

After `continue --json`, use its run ID to bind a check to the exact continued child:

```bash
driftctl verify --run <run-id> \
  --requirement <requirement-id> --json -- <program> [args...]
```

A passing check appends candidate-bound evidence to the requirement. A later bound check detects candidate drift, invalidates stale evidence, and reopens the requirement. Run each aggregate gate with its own deterministic command:

```bash
driftctl verify --run <run-id> --gate regression --json -- <program> [args...]
driftctl verify --run <run-id> --gate integration --json -- <program> [args...]
driftctl verify --run <run-id> --gate protected_scope --json -- <program> [args...]
driftctl verify --run <run-id> --gate review --json -- <program> [args...]
```

The review program must fail when an unresolved `Critical` or `Required` finding exists. A recorded review cannot be rerun on the unchanged candidate; change the candidate first, then rebuild the evidence made stale by that change. The final JSON reports `verified_completion` and concrete missing, failed, stale, or projection blockers. All passing gate records must match the current candidate digest; the child goal binding was established by the verified clear/set/read-back transaction before its turn started.

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

The evaluator command runs the complete deterministic evaluator suite; it does not invoke Codex. The release test builds and installs the real binary locally and checks corrupted-archive rejection.

## Local install rehearsal

Rehearse the release archive, sidecar checksum, and installer without modifying the user's normal binary directory:

```bash
export DRIFTCTL_REHEARSAL=/tmp/driftctl-reproduction
mkdir -p "$DRIFTCTL_REHEARSAL/releases/v0.1.0" \
  "$DRIFTCTL_REHEARSAL/bin"
sh scripts/package-release.sh \
  --out "$DRIFTCTL_REHEARSAL/releases/v0.1.0"
DRIFTCTL_BASE_URL="file://$DRIFTCTL_REHEARSAL/releases" \
  sh scripts/install.sh --version v0.1.0 \
    --bin-dir "$DRIFTCTL_REHEARSAL/bin"
"$DRIFTCTL_REHEARSAL/bin/driftctl" --help
```

The installer requires an explicit version, verifies the archive SHA-256, rejects unexpected archive entries, stages the executable, and invokes `--help` before replacement. The default public release URL is not usable until the repository publishes matching release artifacts; Homebrew remains unverified and unshipped. The installed command uses the user's Codex authentication only when a live provider command runs.

## Frozen five-case native suite

This invokes Codex, consumes the executing user's allowance, and is nondeterministic in latency and model output. It runs cases 01–05 once in frozen order, adds the plain-summary control only to case 02, retains every outcome, and stores raw artifacts outside Git.

```bash
mkdir -p /tmp/driftctl-reproduction/suite-results \
  /tmp/driftctl-reproduction/suite-artifacts
PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_native_suite.py \
  --manifest evals/calibration/manifest.json \
  --results-dir /tmp/driftctl-reproduction/suite-results \
  --driftctl-bin "$DRIFTCTL_REHEARSAL/bin/driftctl" \
  --codex-bin codex \
  --artifacts /tmp/driftctl-reproduction/suite-artifacts
```

The recorded 2026-08-30 run used Luna Max, never-approve, workspace-write candidates, and 128 KiB of post-steering context. Four cases reached A/C comparison: baseline 3/4 and Driftctl 2/4. Case 02's B control also verified. Case 05 required operator clarification before coding and is excluded from completion denominators. Nine candidates passed process, fixed verifiers, and exact scope; independent review blocked three with Required findings. See [`evals/results/native-suite-20260830/summary.json`](evals/results/native-suite-20260830/summary.json). These are descriptive results without a significance claim.

Re-score the immutable arm files without overwriting the published summary:

```sh
python3 evals/runner/score_results.py \
  evals/results/native-suite-20260830/*-native-*.json
```

The scorer treats independent review, fixed verifiers, and scope as external acceptance facts; internal Driftctl closure is never efficacy proof. Each secondary metric includes an availability count. Historical intervention, per-arm projection-fidelity, and token data remain unavailable instead of being inferred as zero. The native runner has a 1,800-second outer process limit; both coding arms use the same provider-terminal-event policy, which is parity evidence rather than a per-arm wall-clock deadline.

## Native case-02 compaction-parity replay

This is the exact one-case command shape used for the eligible 800 KiB native result. It creates real Codex sessions and private artifacts; it is not deterministic in cost, latency, or model output.

```bash
mkdir -p /tmp/driftctl-reproduction/native-results \
  /tmp/driftctl-reproduction/native-artifacts
PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_native_long_session.py \
  --case evals/cases/02-steering-pagination \
  --results-dir /tmp/driftctl-reproduction/native-results \
  --driftctl-bin "$DRIFTCTL_REHEARSAL/bin/driftctl" \
  --codex-bin codex \
  --context-bytes 819200 \
  --worker-model gpt-5.6-luna \
  --worker-effort max \
  --artifacts /tmp/driftctl-reproduction/native-artifacts
```

The recorded pair took 528.933 seconds. Private telemetry peaked near 240,000 input tokens before native compaction, then continued near 32,000 baseline and 27,000 workflow input tokens. Both arms passed their external and exact-scope checks. This is one parity/no-harm observation, not evidence of a correctness improvement, five intact-session pairs, or statistical significance.

## No-TTY conflict safety replay

```bash
mkdir -p /tmp/driftctl-reproduction/conflict-artifacts
PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_conflict_gate.py \
  --results-file /tmp/driftctl-reproduction/conflict-gate.json \
  --driftctl-bin "$DRIFTCTL_REHEARSAL/bin/driftctl" \
  --codex-bin codex \
  --artifacts /tmp/driftctl-reproduction/conflict-artifacts
```

The recorded successful run took 62.159 seconds and used 13,699 input, 3,251 output, and 2,944 reasoning-output tokens. It exited `2`, emitted source-linked alternatives, created no child/candidate, and preserved the source session and workspace. This is one fail-closed safety acceptance case only.

## Archived hard-loss evidence

The committed five-case `baseline`/`workflow` JSON files are retained historical fault-injection evidence: fresh sessions received a surviving worktree after hard transcript loss, with the workflow retaining a durable task record. They recorded 3/5 verified baseline cases and 5/5 workflow cases, took about 35 minutes sequentially, and reported 5,527,378 input-plus-output tokens across both modes. They predate exact mutation-scope scoring and are intentionally rejected by the current fail-closed scorer. Do not combine them with native results or describe them as intact-session efficacy evidence.

For any live run, retain raw artifacts outside the repository, use [`evals/runner/sanitize_trajectory.py`](evals/runner/sanitize_trajectory.py) before publishing selected trajectories, and manually inspect the sanitized output for private paths or context. Do not overwrite case contracts, graders, or existing result files; store independent reruns under new filenames.
