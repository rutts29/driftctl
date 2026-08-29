# Implementation Plan: driftctl

## Overview

Deliver the narrowest useful continuity workflow first, then add one real agent path and the fair evaluation needed by the hackathon.

## Architecture Decisions

- Adapt behavior, not source architecture: the public implementation has no dependency on the private harness.
- The append-only ledger is the durable source of truth; reconstructed state is derived.
- Native agent session resume is optional. The complete goal must be recoverable without it.
- Initial release supports Linux and one verified Codex path.

## Task List

### Phase 1: Continuity

- [x] Task 1: Prove steering survives reopen and blocks premature closure.
- [x] Task 2: Expose the continuity behavior through a stable CLI.

### Checkpoint: Continuity

- [x] Focused and full tests pass.
- [x] The CLI demonstrates start → steer → resume → blocked close → evidenced close.

### Phase 2: Agent execution

- [x] Task 3: Run one Codex task and capture its trajectory without changing repository instructions.
- [ ] Task 4: Run the same task through the direct-agent baseline.

### Checkpoint: Execution

- [ ] One realistic case completes through both paths.
- [ ] Configuration files are unchanged before and after both runs.

### Phase 3: Evaluation and delivery

- [ ] Task 5: Add five cases, deterministic scoring, and complete recorded results.
- [ ] Task 6: Add clean-environment reproduction and Linux release packaging.

### Checkpoint: Complete

- [ ] Hackathon deliverables are present and reproducible.

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Extraction inherits private-harness complexity | High | Reimplement only behavior exercised by the five cases |
| Baseline does not fail | High | Prove the flagship comparison before adding more cases |
| Live runs are nondeterministic | Medium | Use deterministic external scoring and publish all results |
| Portability claim outruns evidence | Medium | Claim one verified adapter and a harness-neutral record |

## Open Questions

- None blocking the first continuity slice.
