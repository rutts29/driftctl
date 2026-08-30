# driftctl

`driftctl` is a local Rust CLI for inspecting an existing Codex session, building a bounded active-intent projection, comparing isolated child continuations, and continuing an approved child. It is a continuity layer, not a replacement for Codex, repository instructions, skills, permissions, or a security sandbox.

## Current local flow

Prerequisites: Git, Rust/Cargo 1.97.1, and an authenticated Codex CLI. Python 3.11+ is required only for evaluation and release tests.

Build and install from source:

```bash
cargo build --release --locked
cargo install --path . --locked
```

From the source repository associated with an existing Codex session:

```bash
driftctl inspect codex --last
driftctl bundle --run <run-id> --json
driftctl compare codex --last
driftctl continue codex --last
```

`inspect` reads the selected Codex session and source repository, then stores immutable history, projection, proposals, and private artifacts under `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`. It does not use a repository ledger for this flow. `bundle` exports a versioned, sanitized projection and blocker bundle that another harness wrapper can attach as context. The implemented native adapter is Codex CLI only.

`compare` creates isolated, equal starting children and leaves adoption manual. `continue` creates one isolated child; ambiguous steering or native-goal changes require an explicit operator action, and no-TTY operation blocks rather than prompting. Existing `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, and the user's harness configuration remain authoritative and unchanged.

A completed child is not verified completion. `continue --json` returns its `child_cwd`, changed paths, current evidence, and blockers. Run requirement-specific checks against that candidate before manually adopting it:

```bash
driftctl verify \
  --candidate <child_cwd> \
  --requirement <requirement-id> \
  --json -- <verification-command> [args...]
```

`verify` records command, verifier, candidate, stdout, and stderr digests while retaining raw output only in private local artifacts.

Driftctl has no service and no telemetry. Provider calls use the user's existing Codex authentication and usage allowance. Source session, source worktree, and parent native goal are read-only; candidate edits occur in an isolated workspace. Isolation is workspace-only: inherited host-wide or YOLO permissions remain outside Driftctl's containment guarantee.

Older `start`, `steer`, `resume`, `verify`, and `close` commands remain available for the legacy local-ledger workflow; they are not the current native-session flow above.

## Evidence and limits

The archived five-case result is hard-transcript-loss fault-injection evidence only: the workflow verified 5/5 cases and the worktree-only baseline 3/5. It does not measure intact native-session continuation, ordinary compaction, or a statistically significant effect; the archived files predate the current exact mutation-scope scorer and are retained rather than rescored as current native evidence.

The eligible native evidence is one case-02 pair at 800 KiB of post-steering context. Both arms recorded native compaction, passed the external and exact-scope checks, and completed in 528.933 seconds. This is compaction-boundary parity/no-harm evidence only. It does not prove an intact-session efficacy improvement, and there are not five intact native pairs.

One separate no-TTY conflict safety case passed: `continue` exited blocked before child creation and preserved the source session and workspace. That single case demonstrates fail-closed handling of its unresolved conflict, not coding-quality improvement or significance. See [`evals/results/conflict-gate.json`](evals/results/conflict-gate.json), [`IMPROVEMENT-CHANGELOG.md`](IMPROVEMENT-CHANGELOG.md), and [`REPRODUCING.md`](REPRODUCING.md).

The native runner uses `gpt-5.6-luna` at `max` reasoning by default. Recorded environment versions include Rust/Cargo 1.97.1, Python 3.14.4 (3.11+ required), Git 2.53.0, and `codex-cli 0.150.1`; live behavior, timing, and usage depend on the executing user's Codex account and configuration.

## Checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s evals/tests -p 'test_*.py' -v
shellcheck scripts/package-release.sh scripts/install.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  release.tests.test_installer -v
```

The first unittest command runs all 19 deterministic evaluator tests. The release test builds an archive, installs the actual binary after SHA-256 verification, invokes `--help`, and rejects a corrupted archive. The public release URL and Homebrew formula are not live yet. See [`REPRODUCING.md`](REPRODUCING.md) for exact rehearsals and live native/conflict commands. Raw provider trajectories can include private context or local paths; keep them outside the repository and sanitize plus manually review anything selected for publication.
