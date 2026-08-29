# Task List

## Task 1: Durable steering blocks premature closure

**Description:** Persist an initial goal, requirement evidence, and later steering in JSONL; reconstruct after reopen and reject closure while the steered requirement remains unresolved.

**Acceptance criteria:**

- [x] Reopening the ledger reconstructs every original and steered requirement.
- [x] Closure reports the exact unresolved requirement IDs.
- [x] Supplying evidence for the remaining requirement permits closure.

**Verification:**

- [x] `cargo test --locked --test continuity`
- [x] `cargo test --locked`

**Dependencies:** None

**Files likely touched:** `src/lib.rs`, `src/ledger.rs`, `tests/continuity.rs`

**Estimated scope:** Medium

## Task 2: Continuity CLI

**Description:** Expose the proven core through start, steer, satisfy, status, and close commands with stable exit behavior.

**Acceptance criteria:**

- [x] Commands operate only on the local namespaced state directory.
- [x] Status is machine-readable and human-readable.
- [x] Failed closure exits non-zero and identifies blockers.

**Verification:**

- [x] `cargo test --locked --test cli`
- [x] `cargo test --locked`

**Dependencies:** Task 1

**Files likely touched:** `src/main.rs`, `src/cli.rs`, `tests/cli.rs`

**Estimated scope:** Medium

## Checkpoint: Tasks 1–2

- [x] All tests and checks pass.
- [x] The core interruption flow works end to end.

## Task 3: Codex execution path

**Acceptance:** One real run captures normalized events while existing repository instructions and permissions remain unchanged. **Complete.**

**Verify:** `cargo test --locked --test codex_adapter` plus `evals/trajectories/codex-smoke.sanitized.jsonl` and `evals/results/codex-smoke.json`.

**Dependencies:** Task 2

## Task 4: Fair baseline path

**Acceptance:** The same task, model, tools, repository, and limits run without continuity enforcement.

**Verify:** Baseline output is accepted by the same external scorer.

**Dependencies:** Task 3

## Task 5: Five-case evaluation

**Acceptance:** All baseline and workflow results, including failures, are recorded and scored.

**Verify:** Evaluation command reproduces the published result table.

**Dependencies:** Task 4

## Task 6: Reproduction and release

**Acceptance:** A clean Linux environment can install the CLI and reproduce the main result without the private harness.

**Verify:** Clean-container rehearsal and release artifact checksum.

**Dependencies:** Task 5
