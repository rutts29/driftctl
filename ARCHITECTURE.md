# Architecture: Same-Session Keeper

## Hard Boundary

```text
install explicit-only skill + isolated hooks (inert)
  → exact $driftctl on at UserPromptSubmit
  → bind Codex-supplied session_id + cwd
  → bootstrap exact persisted session
  → bootstrap private ledger + bounded projection
  → UserPromptSubmit
      → unenrolled: no-op
      → enrolled: reconcile → keeper proposal → validate
          → accepted: commit → inject projection → normal agent turn
          → conflict/failure: persist blocker → stop before model
  → Stop: record non-authoritative outcome + cursor
  → PreCompact: flush
  → SessionStart(resume|compact): recover → reconcile → inject
  → exact $driftctl status → read-only control result
  → exact $driftctl off → detach invoking session → no-op
```

## Components

| Component | Owns | Cannot own |
|---|---|---|
| Codex plugin | Explicit-only skill and lifecycle declarations | Enrollment inference, semantic state, or authority |
| Control parser | Exact `$driftctl on|off|status` grammar | Natural-language activation or semantic intent |
| Hook controller | Enrollment check, locking, orchestration, hook output | Intent invention |
| App Server adapter | Thread read and same-session native-goal read-back | Same-session goal mutation or semantic decisions |
| Source cursor | Ordered provider record identity and replay position | Projection content |
| Keeper worker | Structured proposal from projection plus delta | Ledger writes, goal mutation, approval |
| Proposal validator | Source accounting, lifecycle legality, bounds, stale checks | Provider execution |
| Intent history | Immutable source-linked semantic transitions | Active prompt selection |
| Active projection | Bounded current goal, invariants, frontier, conflicts, closure | Immutable history |
| Enrollment store | Exact provider/session activation | Repository-wide activation |

## Hook Map

| Event | Enrolled action | Output |
|---|---|---|
| `UserPromptSubmit` | reconcile; resolve prompt delta; validate; commit | projection context or blocking reason |
| `Stop` | record assistant/process digest; advance cursor | continue normally |
| `PreCompact` | flush state | continue or block on durability failure |
| `SessionStart: startup|resume` | recover; reconcile; inject | projection context |
| `SessionStart: compact` | recover after native compaction; inject immediately | projection context |

## State

```text
${XDG_STATE_HOME:-$HOME/.local/state}/driftctl/
├── enrollments/<session-digest>.json
└── repositories/<repository-digest>/runs/<run-id>/
    ├── .writer.lock
    ├── source.json
    ├── projection.json
    ├── pending.jsonl
    ├── history/*.jsonl
    ├── proposals/goal-change.json
    └── observations/hook-prompts.jsonl
```

- Session ID remains private; filenames use its digest.
- One writer per attached session.
- Separate session IDs never share writable state.
- Commit: validate base revision/head → atomic projection write → append/rotate history → advance cursor.
- Recovery: accepted projection + immutable history + pending tail + App Server reconciliation.
- Duplicate hook delivery is idempotent by exact enrollment and hook turn ID. A digest-only receipt binds once to Codex's later persisted provider record ID, which may differ from the hook turn ID; later identical prompts cannot reuse that binding.

## State Machine

```text
detached
  └─ exact $driftctl on or external attach ─► attaching
       ├─ bootstrap failure ─► detached
       └─ durable commit ─► attached
            ├─ accepted steering ─► attached
            ├─ ambiguity/goal mismatch ─► blocked
            │    ├─ reject/edit ─► blocked
            │    └─ operator goal update + verified approval ─► attached
            ├─ worker/durability failure ─► blocked
            ├─ restart/compact ─► recovering ─► attached|blocked
            └─ exact $driftctl off or external detach ─► detached
```

## Authority

```text
user prompt             → intent authority
operator resolution     → conflict/goal authority
keeper proposal         → advisory
assistant/tool output   → observation/evidence only
validator + state store → commit authority
Codex App Server        → provider state authority
```

## Existing Code Classification

| Existing capability | New role |
|---|---|
| Codex importer/App Server | live reconcile and goal adapter |
| source cursor | missed/duplicate event control |
| intent history | canonical semantic ledger |
| semantic resolver | replaceable keeper worker |
| active projection | injected context |
| goal-change store | operator proposal state |
| `inspect` | recovery/debugging |
| `continue` | isolated recovery/evaluation |
| `compare` | evaluation only |
| verification | optional closure evidence |

## Failure Rules

| Failure | Attached session behavior |
|---|---|
| Unenrolled session | no-op |
| Unknown/wrong session | reject; no state created |
| Keeper failure | block with retry/detach |
| Invalid proposal | block; no commit |
| Conflict | block; preserve alternatives and sources |
| Projection overflow | block; no silent truncation |
| Stale operator approval | reject |
| Goal read-back mismatch | remain blocked |
| Torn tail/lock conflict | recover verified prefix or block |
| Plugin removed/detached | normal Codex behavior |
