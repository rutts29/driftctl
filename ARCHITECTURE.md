# Architecture: driftctl

## Status

- Target MVP architecture; not a statement of current implementation.
- Current implemented behavior and measured pilot: `README.md`.
- Implementation order and completion gates: `tasks/plan.md`.

## Document Ownership

| Concern | Source of truth |
|---|---|
| User, scope, behavior, acceptance | `SPEC.md` |
| Runtime components, state, interfaces, transitions | `ARCHITECTURE.md` |
| Dependency order, gates, cuts, parallel lanes | `tasks/plan.md` |
| Current public use and measured pilot | `README.md` |
| Pilot reproduction | `REPRODUCING.md` |
| Experiment history | `IMPROVEMENT-CHANGELOG.md` |

## Runtime Topology

```text
┌──────────────────────────── source boundary ────────────────────────────┐
│ Codex session store       source repository       native parent goal │
│ read-only                 read-only               read-only           │
└───────────┬──────────────────────┬──────────────────────┬────────────┘
            │                      │                      │
            ▼                      ▼                      ▼
     provider importer       workspace snapshot       goal observer
            │                      │                      │
            ▼                      │                      │
     normalized events              manifest                  state
            │                      │                      │
            ▼                      │                      │
     immutable history               │                      │
            │                      │                      │
 current projection + delta          │                      │
            │                      │                      │
            ▼                      │                      │
 semantic resolver → validator → conflict/goal gate ←───────────────┘
                                    │
                     accepted or pre-authorized
                                    │
                 ┌──────────────────┴──────────────────┐
                 ▼                                     ▼
        baseline child/workspace                projected child/workspace
                 │                                     │
                 └──────────────────┬──────────────────┘
                                    ▼
                    external verifier + scope review
                                    │
                                    ▼
                         result + sanitized export
```

## Mutation Matrix

| Resource | `inspect` | `compare` | `continue` | Owner |
|---|---:|---:|---:|---|
| Parent session transcript | read | read | read | provider |
| Parent native `/goal` | read when available | read | read | provider |
| Source worktree | read | read | read | user |
| Harness instructions/config | read through provider | read through provider | read through provider | user/provider |
| Driftctl state | write | write | write | Driftctl |
| Baseline temp workspace | — | write | — | Driftctl child |
| Projected temp workspace | — | write | write | Driftctl child |
| Child provider session | — | persisted, disposable | persisted | provider |
| Merge/push/publish | — | — | — | user |

## Local State Layout

```text
${XDG_STATE_HOME:-$HOME/.local/state}/driftctl/
└── repositories/<repo-id>/
    └── runs/<run-id>/
        ├── manifest.json
        ├── source.json
        ├── projection.json
        ├── pending.jsonl
        ├── history/
        │   └── <segment>.jsonl
        ├── proposals/
        ├── results/
        └── trajectories/

${TMPDIR}/driftctl/<run-id>/
├── baseline/
└── workflow/
```

### State rules

- Repository ID: stable digest of canonical repository identity; path omitted from public export.
- Run ID: local opaque identifier.
- `manifest.json`: schema version, provider, source head, workspace digest, model policy, timestamps.
- `source.json`: local provider/session locator; excluded from sanitized export.
- `pending.jsonl`: events after latest accepted projection.
- `history/`: immutable accepted segments; excluded from model context.
- `projection.json`: atomically replaced bounded active state.
- `proposals/`: pending, accepted, rejected, stale goal/projection decisions.
- Single writer: exclusive run lock; concurrent writer exits with deterministic error.
- Commit order:
  1. validate pending sequence;
  2. validate proposal;
  3. write temporary projection;
  4. sync temporary projection;
  5. rename projection;
  6. rotate pending segment into history;
  7. open next pending segment.
- Recovery: replay committed projection plus unrotated pending delta.

## Domain Model

### `SourceRef`

| Field | Type | Rule |
|---|---|---|
| `provider` | enum | `codex`, `bundle` |
| `session` | string | local value; redact on export |
| `record` | string | provider event/message identity or ordinal |
| `role` | enum | `user`, `assistant`, `tool`, `system_observation` |
| `content_digest` | string | source-staleness check |

### `IntentItem`

| Field | Type | Rule |
|---|---|---|
| `id` | stable string | never reused |
| `kind` | enum | `outcome`, `constraint`, `invariant`, `scope`, `validation`, `stop_condition` |
| `text` | string | active semantic statement |
| `lifecycle` | enum | `active`, `superseded`, `withdrawn`, `conflicted` |
| `evidence_state` | enum | `unresolved`, `satisfied`, `reopened` |
| `introduced_by` | `SourceRef[]` | nonempty |
| `changed_by` | `SourceRef[]` | nonempty after transition |
| `supersedes` | `IntentId[]` | explicit edges only |
| `evidence` | `EvidenceRef[]` | requirement-specific |

### `GoalRevision`

| Field | Type | Rule |
|---|---|---|
| `revision` | integer | monotonic |
| `text` | string | current approved objective |
| `source_refs` | `SourceRef[]` | nonempty |
| `supersedes_revision` | integer/null | previous accepted revision |
| `approval` | object/null | required for ambiguous/native-goal change |

### `ActiveProjection`

| Field | Type | Rule |
|---|---|---|
| `schema_version` | integer | exact supported version |
| `revision` | integer | monotonic |
| `source_head` | object | source record and pending sequence |
| `goal` | `GoalRevision` | exactly one active goal |
| `preserve` | `IntentItem[]` | satisfied active invariants/constraints |
| `frontier` | `IntentItem[]` | unresolved active work |
| `validation` | `IntentItem[]` | checks and evidence mapping |
| `conflicts` | `Conflict[]` | unresolved only |
| `closure` | object | deterministic blockers |
| `overflow` | object | explicit count/bytes/reason |
| `generated_by` | object | model, reasoning, prompt/schema versions |

### `ProjectionProposal`

| Field | Type | Rule |
|---|---|---|
| `base_projection_revision` | integer | must equal current revision |
| `base_event_sequence` | integer | must equal pending head |
| `classification` | enum | `no_change`, `additive`, `supersession`, `withdrawal`, `conflict`, `goal_change` |
| `adds` | item proposals | source-linked |
| `supersedes` | transition proposals | source-linked |
| `withdraws` | transition proposals | source-linked |
| `reopens` | transition proposals | source-linked |
| `conflicts` | conflict proposals | at least two alternatives when ambiguous |
| `proposed_goal` | goal/null | required for `goal_change` |
| `preserves` | intent IDs | every retained active item accounted for |
| `confidence` | bounded number | routing hint; never authority |
| `needs_operator_approval` | boolean | validator may promote `false` to `true` |

### `GoalMigrationProposal`

| Field | Rule |
|---|---|
| `proposal_id` | unique local ID |
| `base_projection_revision` | stale-approval guard |
| `base_event_sequence` | stale-approval guard |
| `current_native_goal` | observed value or `unknown` |
| `current_projected_goal` | accepted projection value |
| `conflicting_sources` | nonempty source refs |
| `proposed_goal` | full replacement goal |
| `superseded_items` | explicit IDs |
| `preserved_items` | explicit IDs |
| `rationale` | concise model explanation |
| `approval` | absent, operator-approved, pre-authorized |

## Event Model

| Event | Effect | Required validation |
|---|---|---|
| `run_started` | create source/run identity | first event; unique run |
| `source_imported` | advance imported source head | monotonic provider position |
| `requirement_added` | create intent item | user/approved authority; source exists |
| `requirement_superseded` | deactivate old; activate replacement | old active; explicit edge |
| `requirement_withdrawn` | deactivate item | old active; user/approved authority |
| `conflict_raised` | block affected intent | alternatives and sources exist |
| `conflict_resolved` | apply approved resolution | matching live proposal |
| `goal_revised` | advance projected goal | source-linked; approval when required |
| `evidence_attached` | satisfy mapped requirement | verifier/reviewer source exists |
| `evidence_invalidated` | reopen requirement | prior evidence exists; reason exists |
| `projection_committed` | advance bounded projection | proposal fully validated |
| `closure_attempted` | record blockers/result | closure predicate evaluated |
| `run_closed` | finalize run | zero blockers |

## Projection Invariants

- Exactly one active goal.
- Every active item appears in `preserve`, `frontier`, `validation`, or `conflicts`.
- Every inactive item has an explicit terminal transition.
- Every synthesized statement has valid source refs.
- Every referenced source digest matches imported content.
- Every supersession edge points from newer accepted intent to older active intent.
- Every active conflicted item blocks continuation.
- Every overflow blocks continuation.
- Every projection commit accounts for all previously active IDs.
- Projection byte count includes rendered prompt payload.
- Budget failure returns structured overflow; truncation is invalid.

## Compaction Pipeline

```text
initial import:
  source records
    → validate documented provider items
    → private ordered role/digest cursor
    → coalesce non-user items into digest evidence batches
    → chronological bounded user-authoritative chunks
    → empty/current projection + chunk
    → proposal
    → deterministic validation
    → accepted projection

incremental update:
  accepted projection + pending delta
    → no user text: advance source cursor without model call
    → user text: proposal
    → deterministic validation
    → conflict gate
    → projection commit or blocker
```

### Priority order

1. Current accepted goal.
2. Safety and scope invariants.
3. Unresolved conflicts.
4. Unresolved frontier.
5. Validation and stopping conditions.
6. Satisfied behavior to preserve.
7. Descriptive context.

### Model execution

- Provider: installed Codex CLI.
- Sandbox: read-only.
- Output: JSON constrained by local schema.
- Default: Luna Max.
- Optional preset: Terra High.
- Optional Terra effort: Medium.
- Sol: explicit advanced override.
- Input: active projection, bounded delta, schema, source map.
- Non-user input: digest-only batch counts; compaction markers remain non-authoritative.
- Output authority: proposal only.
- Retry: one schema-repair attempt; semantic ambiguity routes to conflict gate.
- Usage: local provider account; record calls/tokens/time when observable.

## Conflict State Machine

```text
new steering
  ├── additive/compatible → validate → commit
  ├── explicit supersession → validate → commit
  ├── ambiguous → proposal → operator edit/approve/reject/cancel
  └── native-goal conflict → goal proposal
                                  ├── interactive approval
                                  ├── unattended preauthorization + unambiguous
                                  └── blocked report
```

### Approval rules

- Per-event approval default for native-goal changes.
- Up-front unattended approval limited to disposable child goal rewrites.
- Ambiguous alternatives remain blocked in unattended mode.
- New source event invalidates pending approval.
- Operator edit creates a new proposal revision.
- Rejection records new steering as rejected; current goal remains active.
- Model confidence cannot waive required approval.

### Child goal transaction

```text
preconditions
  → create/identify child
  → observe child goal
  → pause when provider requires
  → clear child goal
  → set approved goal
  → read back
  → exact normalized equality
  → continue
```

- Programmatic provider support absent: emit commands; stop before continuation.
- Failed clear/set/read-back: child blocked; parent unchanged.
- Codex implementation: App Server `thread/goal/get`, `clear`, `set`, then `get`.
- Codex goal-bearing children are persisted; ephemeral threads reject native goals.
- Codex forked children do not inherit the parent goal; observe and seed explicitly.
- Comparison deletes its persisted disposable children only after evidence export.
- Automatic rollback: excluded.
- Restore-old-goal action: new explicit approval.

## Provider Capabilities

| Capability | Values | Blocked operation when absent |
|---|---|---|
| session discovery | yes/no | `--last` |
| transcript import | yes/no | `inspect` |
| native fork | yes/no | native `compare`/`continue` |
| workdir rebinding | yes/no | isolated fork execution |
| native goal read | yes/no | verified alignment |
| native goal clear/set | yes/no | automatic goal migration |
| JSONL trajectory | yes/no | automated evidence capture |

## Neutral Session Bundle

```json
{
  "schema_version": 1,
  "source": {
    "provider": "codex",
    "session_ref": "local-private-value",
    "repository_id": "local-repository-digest",
    "head": "provider-record-position"
  },
  "native_goal": {
    "state": "known|absent|unknown",
    "text": "value-when-known"
  },
  "records": [
    {
      "id": "provider-record-id",
      "role": "user|assistant|tool|system_observation",
      "content": "normalized-content",
      "content_digest": "digest"
    }
  ]
}
```

### Bundle rules

- Local bundle may contain session locator and full normalized content.
- Public fixture uses synthetic/approved content and redacted locator.
- Unknown fields: reject for current schema unless explicitly forward-compatible.
- Record order: preserved.
- Empty user-content set: invalid for projection.
- Provider-specific tool payload: normalize or retain as opaque evidence; never grant intent authority.

## Workspace Isolation

### Snapshot inputs

- Git HEAD.
- Tracked working-tree bytes.
- Selected untracked, non-ignored files.
- File mode and symlink metadata.
- Exclusions:
  - `.git/` internals;
  - Driftctl state;
  - provider credentials/configuration;
  - explicit secret patterns;
  - evaluator hidden graders.

### Snapshot outputs

- Baseline root.
- Workflow root.
- Identical pre-run manifest digest.
- Source pre/post manifest digest.
- Candidate diff per arm.

### Execution policy

- `inspect`: resolver in read-only sandbox.
- `compare`: provider workspace-write sandbox or external container; identical across arms.
- `continue`: user-selected provider execution policy.
- Driftctl does not add YOLO permission.
- Host-wide provider permission voids containment guarantee.
- Source non-mutation still checked and reported after every run.

## Closure Model

```text
verified =
  agent_process_succeeded
  AND active_conflicts == 0
  AND projection_overflow == false
  AND every(active_requirement -> mapped_evidence_passed)
  AND regression_suite_passed
  AND integration_checks_passed
  AND protected_scope_passed
  AND unresolved_required_review_findings == 0
  AND native_goal_alignment != false
```

- `close` records predicate inputs and result.
- Evidence includes command, exit status, artifact digest, timestamp, requirement IDs.
- Review includes finding ID, severity, source location, status.
- Shared aggregate evidence string: invalid.
- Provider `turn.completed`: process fact only.

## Evaluation Architecture

### Checkpoint branch experiment

| Arm | Native history | Workspace | Continuation prompt | Added context |
|---|---|---|---|---|
| A: native | same checkpoint | matching manifest | same neutral prompt | none |
| B: plain summary | same checkpoint | matching manifest | same neutral prompt | information-matched flat summary |
| C: Driftctl | same checkpoint | matching manifest | same neutral prompt | bounded active projection |

### Claim boundaries

- Five cases: descriptive evidence.
- Retrospective projection score: fidelity evidence only.
- Recorded continuation: no counterfactual claim.
- Checkpoint fork comparison: conditional causal evidence for tested cases/configuration.
- Driftctl versus plain summary: product-specific evidence.
- Driftctl versus native only: salience-intervention evidence.
- Closure gate: deterministic enforcement evidence; not correctness truth.

## Failure Semantics

| Failure | Result | Source impact |
|---|---|---|
| ambiguous session selection | block; list candidates | none |
| malformed transcript | block; source location | none |
| resolver schema error | one repair; then block | none |
| invented/missing provenance | reject proposal | none |
| projection overflow | block; report budget/items | none |
| unresolved intent conflict | prompt or unattended report | none |
| stale approval | reject; regenerate proposal | none |
| unsupported goal mutation | manual child handoff | none |
| partial child goal migration | block child | none |
| unequal workspace manifests | invalidate comparison | none |
| agent process failure | retain trajectory; fail arm | none |
| verifier failure | retain candidate; block closure | none |
| source post-hash mismatch | critical failure; invalidate run | detected mutation |
| sanitizer failure | retain private artifact only | none |
| ephemeral Codex goal request | reject configuration; require persisted child | none |

## Architecture Decisions

| Decision | Selected | Rejected/deferred | Reason |
|---|---|---|---|
| Product form | local Rust CLI | service, MCP, provider plugin | lowest integration and trust burden |
| Source relation | independent adaptation | public mirror of private harness | reproducibility and scope |
| Native scope | Codex adapter | multi-provider MVP | 48-hour constraint |
| Portability | neutral JSON bundle | universal native claim | stable minimal boundary |
| Continuity mechanism | native history plus salience projection | replacement summary | target is drift with history present |
| Storage | immutable history plus bounded projection | ever-growing prompt ledger | auditability without prompt growth |
| Semantic authority | model proposal plus validation/approval | model self-authority | conflict safety |
| Conflict default | interactive | silent latest-wins | ambiguous user intent |
| YOLO support | child-only preauthorization | parent mutation | autonomy with recoverability |
| Evaluation | checkpoint branch comparison | retrospective outcome backtest | counterfactual validity |
| Main case count | five strong cases | ten shallow cases | weekend quality constraint |
| Distribution | release + verified curl + Homebrew | npm MVP | native binary fit |
| Security | source isolation and explicit limits | full security harness | product focus and schedule |
