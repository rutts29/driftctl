# Spec: driftctl

## Objective

Build a small standalone CLI for developers using coding agents on long-running tasks. It must preserve the complete goal and later steering across interrupted sessions, expose the current unresolved frontier, and refuse verified completion until every requirement has evidence.

This is an adapted public implementation of one continuity mechanism from a broader private engineering harness. It is intentionally not a replica and must not depend on the private repository.

## Tech Stack

- Rust 2024 edition, Rust 1.97.1
- `serde` and `serde_json` for the versioned JSONL record
- Standard-library CLI and filesystem operations unless evidence justifies another dependency

## Commands

- Build: `cargo build --locked`
- Focused test: `cargo test --locked --test continuity`
- Full test: `cargo test --locked`
- Format: `cargo fmt --check`
- Lint: `cargo clippy --locked --all-targets -- -D warnings`

## Project Structure

- `src/` — continuity core and CLI
- `tests/` — behavior and command-level tests
- `evals/` — baseline runner, cases, scoring, and recorded results
- `tasks/` — active implementation plan and checklist

## Code Style

Prefer small typed state transitions and explicit errors:

```rust
match event {
    Event::SteeringAdded { requirement } => state.add(requirement)?,
    Event::RequirementSatisfied { id, evidence } => state.satisfy(id, evidence)?,
}
```

Use one house style across extracted and new code: `rustfmt` layout, explicit typed state, small named helpers, stable domain terms, and behavior-first integration tests. Forbid unsafe code and avoid abstractions that do not serve the evaluated workflow. Reuse proven private-harness behavior when adapting it is faster than rewriting it, but do not import unrelated infrastructure dependencies.

## Testing Strategy

- Unit tests cover deterministic event folding and closure decisions.
- Filesystem integration tests prove append, interruption, reopen, and resume.
- Command tests prove stable exit codes and machine-readable output.
- Live agent evaluation is kept separate from deterministic tests.

## Boundaries

- Always: preserve append order, validate external records, use the same cases for baseline and workflow, and bind completion to evidence.
- Ask first: add a runtime dependency, expand supported agent harnesses, or change the public command contract.
- Never: modify a user's `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, or global harness configuration; copy secrets or raw private trajectories; depend on the private harness at runtime.

## Success Criteria

- Steering added after work begins survives process interruption and resume.
- A completion attempt fails while any original or steered requirement lacks evidence.
- One Codex-backed task runs end to end without modifying existing agent instructions.
- Baseline and workflow run against the same five cases and produce complete scored results.
- A clean Linux environment can install and reproduce the main result from this repository alone.

## Open Questions

- Public project name and license must be finalized before release; `driftctl` is the working name.
