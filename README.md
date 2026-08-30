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

For another harness, export its approved local transcript into the strict schema-v1 neutral session bundle defined in [`SPEC.md`](SPEC.md), set `provider` to `bundle`, and run:

```bash
driftctl inspect bundle --file session.json --json
# or: harness-export-command | driftctl inspect bundle --stdin --json
```

The bundle's repository digest must match the canonical current repository. File/stdin intake is capped at 16 MiB, validates every record/digest before model use, and keeps non-user roles non-authoritative. Re-reading the same bundle uses cached state without another model call. This release treats a changed bundle as a new snapshot and requires a new `session_ref`; it does not claim native control of the source harness.

`inspect` reads the selected Codex session or neutral bundle and source repository, then stores immutable history, projection, proposals, and private artifacts under `${XDG_STATE_HOME:-$HOME/.local/state}/driftctl`. It does not use a repository ledger for this flow. `driftctl bundle --run` exports a separate sanitized projection/blocker handoff that another harness wrapper can attach as context. The implemented native process adapter is Codex CLI only.

`compare` creates isolated, equal starting children and leaves adoption manual. `continue` creates one isolated child; ambiguous steering or native-goal changes require an explicit operator action, and no-TTY operation blocks rather than prompting. Existing `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, and the user's harness configuration remain authoritative and unchanged.

If the installed Codex App Server cannot complete a child goal clear/set/read-back, `continue --json` exits `2` without starting the child turn. It returns the child ID/CWD, observed and intended goal states, `codex resume <child-id>`, and separate `/goal clear` plus `/goal` instructions. The operator must grant new approval and verify the child goal manually; Driftctl never runs those interactive commands itself.

For a blocked semantic conflict, copy the conflict and alternative IDs from `inspect --json`, then make the operator choice explicitly:

```bash
driftctl continue codex --last \
  --resolve-conflict <conflict-id> <alternative-id>
```

The decision is recorded in private local state and only the selected alternative enters the child prompt. Invalid IDs and unattended ambiguity create no child.

A completed child is not verified completion. `continue --json` returns its `run_id`, `child_cwd`, changed paths, current evidence, and blockers. Attach requirement-specific checks to that exact continued child before manually adopting it:

```bash
driftctl verify \
  --run <run-id> \
  --requirement <requirement-id> \
  --json -- <verification-command> [args...]
```

`verify --run` records command, verifier, candidate, stdout, and stderr digests while retaining raw output only in private local artifacts. Passing evidence is appended to that requirement's durable history. If the candidate changes, a later bound check invalidates prior candidate-bound evidence and reopens the requirement. `--candidate <path>` remains available for a standalone check but does not update a run.

Full closure also requires four explicit commands against the same candidate checkpoint:

```bash
driftctl verify --run <run-id> --gate regression --json -- <regression-command>
driftctl verify --run <run-id> --gate integration --json -- <integration-command>
driftctl verify --run <run-id> --gate protected_scope --json -- <scope-command>
driftctl verify --run <run-id> --gate review --json -- <review-command>
```

The review command must exit nonzero when any unresolved `Critical` or `Required` finding exists. Review is one-shot per candidate digest: after it records a failure, the candidate must change before another review, which also makes prior requirement and gate evidence stale. `verified_completion` becomes true only when every active requirement has current evidence, all four gates pass on the same candidate digest, no conflict or overflow remains, and the continued child has the exact goal binding already proved during migration.

Driftctl has no service and no telemetry. Provider calls use the user's existing Codex authentication and usage allowance. Source session, source worktree, and parent native goal are read-only; candidate edits occur in an isolated workspace. Isolation is workspace-only: inherited host-wide or YOLO permissions remain outside Driftctl's containment guarantee.

Older `start`, `steer`, `resume`, `verify`, and `close` commands remain available for the legacy local-ledger workflow; they are not the current native-session flow above.

## Evidence and limits

The frozen intact-session suite ran once at 128 KiB of post-steering context. Four cases reached behavioral comparison: baseline verified 3/4 and Driftctl verified 2/4. The one plain-summary control tied both other arms. Case 05 blocked before coding on a source-linked unresolved conflict and is reported separately from completion rates. This is descriptive negative evidence: it does not show that Driftctl improves coding outcomes.

All nine coding candidates passed the fixed verifiers and exact scope before independent review. One blind, read-only review per candidate found three Required defects and blocked closure without a feedback/fix loop. The useful measured behavior was refusal to claim verified completion, not an efficacy gain from the projection. See [`evals/results/native-suite-20260830/summary.json`](evals/results/native-suite-20260830/summary.json) and [`IMPROVEMENT-CHANGELOG.md`](IMPROVEMENT-CHANGELOG.md).

An earlier case-02 pair at 800 KiB recorded native compaction in both arms, passed the external and exact-scope checks, and completed in 528.933 seconds. This remains separate compaction-boundary parity/no-harm evidence.

One separate no-TTY conflict safety case passed: `continue` exited blocked before child creation and preserved the source session and workspace. That single case demonstrates fail-closed handling of its unresolved conflict, not coding-quality improvement or significance. See [`evals/results/conflict-gate.json`](evals/results/conflict-gate.json), [`IMPROVEMENT-CHANGELOG.md`](IMPROVEMENT-CHANGELOG.md), and [`REPRODUCING.md`](REPRODUCING.md).

The archived five-case hard-loss result remains historical fault-injection evidence only: workflow 5/5 versus worktree-only baseline 3/5. It predates the current exact-scope and independent-review gates and is not combined with the intact-session result.

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

The evaluator unittest command runs the complete deterministic evaluator suite without provider calls. The release test builds an archive, installs the actual binary after SHA-256 verification, invokes `--help`, and rejects a corrupted archive. The public release URL and Homebrew formula are not live yet. See [`REPRODUCING.md`](REPRODUCING.md) for exact rehearsals and live native/conflict commands. Raw provider trajectories can include private context or local paths; keep them outside the repository and sanitize plus manually review anything selected for publication.
