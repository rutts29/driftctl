# Driftctl

Driftctl keeps one opted-in Codex session aligned with its accepted goal and later steering. It runs locally through Codex lifecycle hooks and injects a compact, source-linked active-intent projection before work continues.

Primary flow:

```text
install plugin
  → plugin remains inert
  → user invokes $driftctl on or its Codex-qualified form inside one existing session
  → hook binds the exact session_id supplied by Codex
  → reconcile prompt
  → Luna proposes semantic change
  → deterministic validation commits or blocks
  → inject active intent into the same session
  → recover and inject again after resume or compaction
  → user invokes $driftctl off to restore ordinary Codex behavior
```

The existing `inspect`, `continue`, `compare`, and evaluation commands remain available as recovery and research tools. They are not the same-session keeper.

## Requirements

- Linux x86_64.
- Codex CLI 0.150.1 or a compatible release with stable lifecycle hooks.
- Existing local Codex authentication.
- Git repository associated with the target session.
- Rust/Cargo 1.97.1 only when building from source.

Driftctl uses the user's Codex allowance. The keeper defaults to `gpt-5.6-luna` at `max`; `terra` with `high` or `medium` is optional.

## Install

From source:

```bash
cargo install --path . --locked
driftctl integrate codex install
driftctl integrate codex status
```

The packaged Linux release is checksum-verified by `scripts/install.sh`:

```bash
./scripts/install.sh --version v0.5.1
driftctl integrate codex install
```

The GitHub repository and release remain private until the operator publishes them. After publication, the same installer downloads the pinned archive and checksum from GitHub.

Open Codex once after integration and approve the Driftctl hook source in the `/hooks` review UI. Normal interactive use does not require a trust-bypass flag.

## Enable one existing session

Inside the existing Codex session:

```text
$driftctl on
```

Codex may render the same exact control as `$driftctl-codex:driftctl on`; Driftctl accepts both forms. The corresponding `status` and `off` forms are also supported.

Installation alone never enrolls a session. The literal command is handled by `UserPromptSubmit`, which supplies the exact current session ID and working directory. Other sessions—including sessions in the same repository—remain strict no-ops.

Check or disable that exact session without leaving Codex:

```text
$driftctl status
$driftctl off
```

The controls are explicit-only. Near matches, ordinary prompts, startup, resume, and compaction cannot activate Driftctl. Control messages remain source-accounted but never become semantic task intent.

The external CLI remains available for diagnostics and compatibility:

Copy the persisted Codex session UUID, enter that session's repository, then run:

```bash
driftctl attach codex --session <exact-session-id>
driftctl status codex --session <exact-session-id>
```

Continue that same session normally. Only the exact enabled session is active; other sessions in the same repository remain untouched.

## Run a prospective A/B trial

Start from a detached, persisted midpoint session in a clean source checkout:

```bash
driftctl ab prepare codex --session <midpoint-session-id> --json
```

Or select an earlier completed turn and bind its filesystem checkpoint explicitly:

```bash
driftctl ab prepare codex \
  --session <source-session-id> \
  --through-turn <completed-turn-id> \
  --source-ref <git-commit> \
  --json
```

Both arms receive history through the selected turn only. `source-ref` resolves once to an immutable commit and both candidate repositories start there. If the selected turn is active, prepare creates no experiment, workspace, or fork and reports the preceding completed turn when available. Driftctl cannot reconstruct uncommitted historical files; select a commit that represents the intended checkpoint.

Prepare also rejects a selected history whose provider content or native goal embeds an absolute path into the parent checkout. Codex can follow such inherited paths despite a different fork working directory, so this case fails before A/B state, candidates, or forks are created.

The result contains one run ID plus separate baseline/workflow session IDs and working directories. Both are idle persisted forks with the same inherited prefix, native goal, worker policy, and candidate digest. Prepare enrolls neither arm and does not change the source checkout.

Resume each reported session from its reported directory. Continue the baseline normally. In the workflow arm, invoke `$driftctl on` before giving the same continuation task. Before invoking `$driftctl off`, run one acceptance command against both candidates:

```bash
driftctl ab report --run <run-id> --json -- ./verify-candidate.sh
```

Report requires a detached baseline, the exact enrolled workflow fork, an unchanged source, and the recorded equal starting state. It resolves the verifier program once from the report invocation directory, requires that canonical regular file to remain outside both candidates, hashes its content, and rejects candidate-resolved inputs or verifier mutation. The same verifier runs once per arm; an identical retry is cached. Verified completion is primary; post-checkpoint prompts, records, verifier time, and measured keeper overhead are secondary evidence.

This command prepares and measures the experiment; it does not drive either coding session or adopt either candidate.

On every accepted prompt, Driftctl:

1. Reconciles persisted records missed during interruption.
2. Gives Luna only the bounded active projection plus the new user delta.
3. Validates source accounting, legal transitions, stale bases, and size bounds.
4. Commits the durable ledger and injects the active projection before the model turn.

`Stop` advances the provider cursor without granting assistant or tool output steering authority. `PreCompact` flushes state. `SessionStart` restores and injects the projection on startup, resume, and post-compaction continuation.

Private state is under `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`. There is no daemon, hosted service, telemetry, credential copy, or repository instruction rewrite.

## Resolve a block

Get the run ID and inspect the sanitized active projection:

```bash
driftctl status codex --session <exact-session-id> --json
driftctl bundle --run <run-id> --json
```

Choose an explicit conflict alternative:

```bash
driftctl resolve codex \
  --session <exact-session-id> \
  --conflict <conflict-id> \
  --alternative <alternative-id>
```

Decide a proposed overall goal change:

```bash
driftctl resolve codex --session <exact-session-id> --reject-goal
driftctl resolve codex --session <exact-session-id> --edit-goal '<replacement>'
driftctl resolve codex --session <exact-session-id> --approve-goal
```

If the native goal does not already match, approval exits `2` with two commands for the exact attached Codex session:

```text
/goal clear
/goal <replacement>
```

Run those commands in that session, then rerun `--approve-goal`. Driftctl uses read-only goal checks and commits only after exact source and native-goal read-back. It never clears or sets the same-session native goal. Wrong-session, stale, failed, concurrent, or replayed decisions leave it untouched.

Detach only that session:

```bash
driftctl detach codex --session <exact-session-id>
```

## Verified behavior

- Unenrolled sessions are strict no-ops.
- Plugin installation never enrolls a session; activation requires an exact literal or Codex-qualified Driftctl control.
- `$driftctl status` is read-only and `$driftctl off` detaches only the invoking session.
- Attach is exact and idempotent; unknown sessions create no enrollment.
- Different hook and persisted provider IDs bind one-to-one without duplicate semantic calls.
- Additive steering is committed before model execution.
- Ambiguity, invalid proposals, overflow, and pending goal changes block before work.
- A real attached Codex session retained a new output constraint across a separate resume process.
- A production-shaped `PreCompact → SessionStart(compact)` restored the same goal and constraint.
- A real persisted checkpoint produced two independent forks; only the workflow fork enrolled, both candidates passed the same verifier, and a repeated report reused cached evidence.
- The process suite covers install/remove preservation, isolation, detach, duplicate delivery, invalid output, conflict resolution, and native-goal approval.

Exact commands and retained evidence are in [REPRODUCING.md](REPRODUCING.md). The product contract and component boundaries are in [SPEC.md](SPEC.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Limits

- Codex is the only native lifecycle adapter in this MVP.
- Keeper calls add model latency and consume the user's allowance.
- Historical A/B requires an explicit completed turn and Git commit; Codex rejects an in-progress `lastTurnId`, and uncommitted historical files cannot be reconstructed.
- A/B preparation blocks inherited absolute references into the parent checkout; use a checkpoint without those references rather than relying on the child sandbox to contain them.
- A historical fork inherits the native goal observed at prepare time because Codex does not expose turn-versioned goal history; output labels this as `native_goal_basis: current_at_prepare`.
- A/B execution remains operator-driven; equal starting state does not make asymmetrically steered runs a valid efficacy comparison.
- Model proposals can be wrong; deterministic checks reject structural errors but cannot guarantee semantic correctness.
- Hook trust is an operator decision. Untrusted hooks may be skipped by Codex.
- Driftctl is not a security sandbox and does not make YOLO or host-wide permissions safe.
- The retained frozen coding evaluation is negative: baseline 3/4 versus workflow 2/4. Do not claim coding-efficacy improvement from it.
- The current command name collides with an existing public tool; rename before broad package-manager publication.

## Checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s evals/tests -p 'test_*.py' -v
shellcheck scripts/package-release.sh scripts/install.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest release.tests.test_installer -v
```

Raw provider trajectories and local state can contain private context and paths. Keep them outside the repository; publish only sanitized, manually reviewed evidence.
