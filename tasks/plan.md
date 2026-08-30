# Implementation Plan: Long-Session Drift Resistance

## Contract

- Behavior and acceptance: `SPEC.md`.
- State, interfaces, isolation, evaluation design: `ARCHITECTURE.md`.
- Detailed local work queue: ignored `tasks/LOCAL-IMPLEMENTATION-CHECKLIST.md`.
- Duration: 48 hours.
- Native provider: Codex.
- Portable boundary: neutral JSON bundle.
- Cases: five long-session checkpoints; retain every existing pilot case/result.
- Runtime: local CLI using the user's provider authentication and allowance.
- Scope: continuity and source isolation; no general security harness.
- Completion: focused tests plus one production-shaped run through the shipped CLI and real process/filesystem/permission boundaries.
- Scope freeze: only a concrete failure from that run may add MVP work; an unavailable boundary blocks its gate.

## Tasks

| ID | Deliverable | Depends on | Complete when |
|---|---|---|---|
| T00 | Freeze pilot evidence | — | current tests pass; cases/results retained; pilot rescored |
| T01 | Codex session-source spike | T00 | one existing session imports read-only; malformed fixtures fail |
| T02 | Child-fork/workdir spike | T00 | two isolated children share one source checkpoint; parent unchanged |
| T03 | Native-goal capability spike | T00 | read/clear/set/read-back proven or manual fallback selected |
| T04 | Domain and history v2 | T01, T03 | typed transitions replay deterministically; provenance required |
| T05 | Bounded active projection | T04 | supersession, withdrawal, conflict, evidence, overflow tests pass |
| T06 | Neutral bundle and Codex importer | T01, T04 | strict round trip; source roles and refs preserved |
| T07 | Semantic resolver and compaction | T05, T06 | projection-plus-delta stays bounded; invalid proposals cannot commit |
| T08 | Conflict and goal gate | T03, T07 | ambiguity requires authority; parent goal never changes |
| T09 | Read-only workspace isolation | T02 | paired manifests match; source pre/post manifest matches |
| T10 | Requirement-specific verification | T04, T05 | closure uses mapped evidence and external facts |
| T11 | `inspect` | T06–T08 | one command returns projection/blocker without source mutation |
| T12 | `compare` | T09–T11 | native/projected children run from an equal checkpoint |
| T13 | `continue` and bundle handoff | T08, T09, T11 | resumable child or explicit blocker; adoption remains manual |
| T14 | Five long-session cases | T05–T07 | gold intent, reference/negative patches, stable hidden graders frozen |
| T15 | Evaluation runner and scorer | T10, T12, T14 | fair A/C pairs and flagship B control produce auditable results |
| T16 | Live evaluation | T15 | all valid runs retained; metrics and limitations published |
| T17 | Reproduction and trajectories | T16 | clean one-case run works; publishable artifacts pass manual review |
| T18 | Release packaging | T13 | checksummed binaries, verified installer, Homebrew path work |
| T19 | Submission and acceptance review | T17, T18 | deliverables complete; no Critical/Required finding remains |

## Gates

### G0 — Pilot

- [x] Rust format, lint, and tests pass.
- [x] Evaluator tests pass.
- [x] Existing five-case summary reproduces.
- [x] Existing valid and discarded artifacts remain present.

### G1 — Feasibility

- [ ] T01–T03 complete.
- [ ] Session discovery, fork isolation, and goal capability branches selected from evidence.
- [ ] Human approves manual goal fallback when automation is unsupported.

### G2 — Intent core

- [ ] T04–T08 complete.
- [ ] Active intent cannot disappear without a source-linked transition.
- [ ] Repeated compaction remains bounded.
- [ ] Ambiguous conflict cannot auto-commit.

### G3 — Workflow

- [ ] T09–T13 complete.
- [ ] Install → inspect → compare works.
- [ ] Parent session, goal, source worktree, and harness configuration remain unchanged.
- [ ] Host-wide YOLO limitation is visible.

### G4 — Evidence

- [ ] T14–T17 complete.
- [ ] Five checkpoint pairs are complete or reduced scope is disclosed.
- [ ] Retrospective results are labelled fidelity-only.
- [ ] Pilot and long-session results remain separate.
- [ ] Five-case results are described without significance claims.

### G5 — Ship

- [ ] T18–T19 complete.
- [ ] Clean install and reproduction pass.
- [x] Sanitized artifacts pass manual path/secret review.
- [ ] Every public claim points to submitted evidence.

## Parallel Waves

| Wave | Root | Lane A | Lane B | Lane C |
|---|---|---|---|---|
| 1 | T00, contract ownership | T01 | T02 | T03 |
| 2 | T04 integration | T05 | T06 | T09 |
| 3 | integration | T07 | T10 | T14 |
| 4 | T08 integration | T11 | T12 fixtures | T15 scaffold |
| 5 | G3 | T13 | T15 | T17 scaffold |
| 6 | T16 coordination | evaluation runs | T17 | T18 |
| 7 | T19 review | fixes | evidence audit | packaging audit |

## Cut Order

1. npm wrapper.
2. Native Claude adapter.
3. MCP/provider plugin.
4. Automatic merge/adoption.
5. More than one plain-summary control.
6. More than five new cases.
7. Automatic goal migration when provider support is absent; keep proposal/manual handoff.

## Stop Conditions

- Deterministic active-intent loss: stop reliability claim.
- Unequal checkpoint/workspace: invalidate pair.
- Parent/source mutation: stop non-impact claim.
- Goal without read-back equality: stop migration claim.
- Hidden grader visible or changed: invalidate run.
- Credential/private-path leak: block release.
- Mixed/negative result: publish scoped evidence; retain failures.
