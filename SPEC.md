# Product Contract: driftctl

## Status

- Product: standalone local CLI.
- Target: weekend-hackathon MVP.
- Current proof: manual continuity ledger, Codex process adapter, five-case hard-loss pilot.
- Next proof: drift resistance inside intact long-running sessions.
- Private origin: behavior adapted from a broader engineering harness.
- Runtime dependency on private harness: none.

## User

- Primary: solo developers and open-source maintainers delegating multi-file work to coding agents.
- Context: long-running session; complete history may still exist; active intent has lost salience.
- Pain:
  - later steering loses influence;
  - satisfied constraints regress;
  - stale instructions remain active;
  - native durable goal conflicts with newer steering;
  - agent claims completion before integration evidence exists;
  - user repeats context or abandons the session.

## Promise

- Input: existing local agent session or harness-neutral session bundle.
- Output:
  - bounded active-intent projection;
  - unresolved frontier;
  - source-linked conflict report;
  - isolated child continuation;
  - externally verified candidate result.
- Core flow:

```text
complete history
  → immutable normalized events
  → bounded active projection
  → conflict gate
  → isolated child continuation
  → external verification
  → explicit closure
```

## Required Behavior

### Session intake

- Discover the latest Codex session for the current repository or accept an explicit session ID.
- Read source session files and repository state without mutation.
- Accept a versioned neutral bundle for unsupported harnesses.
- Reject ambiguous session selection.
- Reject unsupported or malformed provider records with source location.
- Record source provenance for every imported intent item.

### Intent model

- Preserve:
  - current accepted goal;
  - original goal provenance;
  - explicit user constraints;
  - latest effective steering;
  - satisfied behavior that must remain true;
  - unresolved work;
  - validation obligations;
  - scope boundaries;
  - stopping condition;
  - unresolved conflicts.
- Separate intent lifecycle from evidence state.
- Intent lifecycle: `active`, `superseded`, `withdrawn`, `conflicted`.
- Evidence state: `unresolved`, `satisfied`, `reopened`.
- Preserve superseded and withdrawn items in immutable history.
- Exclude inactive items from the active prompt projection.
- Link every supersession, withdrawal, conflict, resolution, and goal revision to source records.
- Treat assistant and tool output as evidence or context; require explicit user text or operator approval for intent authority.

### Semantic compaction

- Fold `current projection + new event delta`; avoid replaying full history on each update.
- Keep immutable history outside the agent prompt path.
- Bound active projection by a configured byte budget.
- Preserve goal, invariants, conflicts, unresolved requirements, and stopping condition before descriptive context.
- Surface overflow; never silently drop an active item.
- Block continuation while projection overflow or unresolved semantic conflict remains.
- Validate model-produced projection proposals before commit.
- Require every synthesized clause to cite one or more imported source records.

### Conflict policy

- Additive steering: commit without interruption after validation.
- Explicit compatible supersession: update active projection after validation.
- Ambiguous steering: create proposal; request operator resolution.
- Native-goal conflict: create goal-migration proposal; retain current goal until approval.
- Proposal actions:
  - approve on isolated child;
  - edit proposed goal;
  - retain current goal and reject new steering;
  - cancel.
- Model role: proposal generation.
- Operator role: semantic authority.

### Native goal migration

- Apply goal changes only to a newly created child session.
- For Codex, use a persisted child: ephemeral threads do not support native goals.
- Preserve the parent session and parent `/goal` byte-for-byte.
- Bind approval to exact source sequence and projection revision.
- Invalidate approval after new source events.
- Apply child transition:
  - pause child when required;
  - clear child goal;
  - set approved goal;
  - read back child goal;
  - compare with approved value;
  - continue only after equality.
- On unsupported programmatic goal control:
  - emit exact operator commands;
  - leave child blocked;
  - record no successful migration.
- On partial transition failure:
  - leave child blocked;
  - report observed and intended state;
  - require new approval for restoration or retry.

### Interactive and unattended operation

- Interactive default: prompt on native-goal changes and ambiguous conflicts.
- Unattended opt-in: pre-authorize unambiguous goal migrations inside disposable children.
- Unattended ambiguity: write conflict artifact; exit without waiting for input.
- Provider YOLO setting: execution permission only; no implied semantic authority.
- Parent session: remains available throughout either mode.

### Continuation and closure

- Continue in an isolated child fork when native provider support exists.
- Produce a neutral prompt bundle when native fork support is absent.
- Keep existing repository instructions and user harness configuration active.
- Require requirement-specific evidence.
- Closure predicate:
  - no active conflict;
  - no projection overflow;
  - every active requirement has valid evidence;
  - required regression and integration checks pass;
  - protected-scope check passes;
  - no unresolved `Critical` or `Required` review finding;
  - active goal and child native goal are aligned when native goal is observable.
- Agent completion message: evidence input only; never closure proof.

## UX Contract

### Target MVP commands

- `driftctl inspect codex (--last | --session <id>)`
- `driftctl bundle --run <run-id> --json`
- `driftctl compare codex (--last | --session <id>)`
- `driftctl continue codex (--last | --session <id>)`

### Command defaults

- `--last`: latest session scoped to current repository.
- `inspect`: read-only source analysis; no child continuation.
- `compare`: disposable paired children; no merge or source mutation.
- `continue`: one child; adoption remains an explicit user action.
- `--json`: versioned machine-readable output.
- Missing TTY: never wait on a prompt.

## Model Policy

- Compactor default: `gpt-5.6-luna`, reasoning `max`.
- Terra preset: `gpt-5.6-terra`, reasoning `high`.
- Terra override: reasoning `medium`.
- Sol: explicit advanced override; absent from recommended path.
- Continuation worker: inherit source/user configuration unless an evaluation pins equal settings across arms.
- Before model call, display or emit:
  - selected model;
  - reasoning effort;
  - expected call count;
  - use of the user's existing provider authentication and usage allowance.
- Usage claim: record observed tokens/time; make no unmeasured percentage or price claim.
- Disclosure: model output may be incomplete or wrong; operator owns approved intent.

## Locality and Trust Boundaries

- Driftctl service: none.
- Driftctl telemetry: none.
- State: local XDG state directory by default.
- Provider authentication: consumed by provider CLI; never copied into Driftctl state.
- Source session: read-only.
- Source worktree: read-only.
- Parent native goal: read-only.
- Harness files: read-only.
- Candidate edits: isolated temporary workspace.
- Merge, push, publish, install into target harness: explicit user action outside automatic continuation.
- Driftctl: continuity layer; not a security sandbox.
- Host-wide YOLO permission: outside Driftctl's containment guarantee.
- Strong non-impact evaluation: provider sandbox or external container required.

## Harness Portability

- Verified native adapter for MVP: Codex CLI.
- Portable interface: versioned neutral session bundle and active projection JSON.
- Claude Code, editor, and custom-harness integration: consume bundle; dedicated adapters deferred.
- Existing `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, and global configuration: unchanged.
- MCP server: excluded from MVP.
- Provider plugin: excluded from MVP.

## Evaluation Contract

- Main claim target: bounded active-intent projection improves verified continuation from intact long-running coding sessions under tested conditions.
- Unit of comparison: identical frozen checkpoint.
- Required arms:
  - `A`: native session fork; neutral continuation prompt;
  - `C`: identical native fork and workspace; same prompt plus Driftctl projection.
- Flagship control:
  - `B`: identical fork plus information-matched plain summary.
- Required cases: five high-quality long-session checkpoints.
- Primary metric: externally verified completion rate.
- Primary completion facts:
  - agent process success;
  - upstream tests pass;
  - hidden functional/integration grader passes;
  - protected-scope grader passes;
  - no unresolved critical review finding.
- Secondary metrics: requirement pass rate, projection fidelity, false completion, regressions, scope changes, interventions, elapsed time, tokens.
- Fairness controls:
  - same source session checkpoint;
  - same workspace manifest;
  - same continuation prompt;
  - same worker model and reasoning;
  - same tools, sandbox, timeout, and verifier;
  - projection-generation overhead recorded separately;
  - randomized arm order where supported.
- Existing five-case hard-loss pilot:
  - retained unchanged;
  - labelled pilot/fault-injection evidence;
  - excluded from long-session causal claim;
  - never deleted when superseded by stronger evaluation.
- Retrospective inspection:
  - valid for projection fidelity and conflict detection;
  - invalid as behavioral counterfactual evidence.

## Non-Goals

- General security harness.
- Credential broker.
- VPS provisioning.
- Signing infrastructure beyond release artifact checksums.
- Autonomous merge or push.
- Replacement for native context, repository instructions, skills, tools, or permissions.
- Universal provider adapter claim.
- Statistical significance claim from five cases.
- Guaranteed improvement for every task or model.

## Acceptance Criteria

- Existing Codex session imports without source mutation.
- Active projection stays within budget or reports blocking overflow.
- Superseded and withdrawn intent remains auditable and absent from active prompt.
- Ambiguous conflict produces no silent resolution.
- Approved goal migration affects child only and verifies exact resulting goal.
- Unattended mode never waits and never silently resolves ambiguity.
- Baseline and workflow forks share a frozen workspace manifest.
- Existing instruction and configuration files match before/after hashes.
- Five new checkpoint cases produce complete, inspectable result records.
- Current tests, cases, pilot results, and discarded results remain present.
- Clean environment reproduces deterministic checks and at least one live comparison.
- Public claims match observed evidence.

## Open Decisions

- Public name and license. `driftctl` is internal-only because it collides with an existing CLI and Homebrew formula.
- Exact projection byte budget after stress measurement.

## Release Target

- MVP: `x86_64-unknown-linux-gnu` archive plus SHA-256 sidecar and pinned installer.
- Unsupported targets: reject before build or download.
- Homebrew: deferred until a public rename, macOS artifact, and real `brew` boundary exist.
