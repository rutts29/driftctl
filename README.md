# driftctl

`driftctl` is a small drift-resistant workflow for long-running coding-agent tasks.

It preserves the complete goal and later steering in a durable event ledger, reconstructs the current unresolved frontier after interruption, and requires evidence before reporting verified completion.

This repository is an independent, adapted implementation of one mechanism from a broader private engineering harness. It does not require or modify that harness.

The first standalone continuity flow is working:

```console
$ driftctl start --goal "Add retry support" --requirement "Retry once"
started
$ driftctl steer --requirement "Do not retry authentication failures"
R2
$ driftctl resume
goal: Add retry support
unresolved: R1, R2
closed: false
$ driftctl satisfy --id R1 --evidence "retry unit test passes"
satisfied R1
$ driftctl close
closure blocked; unresolved requirements: R2
$ driftctl satisfy --id R2 --evidence "integration test passes"
satisfied R2
$ driftctl close
verified
```

State is confined to `.driftctl/ledger.jsonl` in the current repository. The CLI does not edit `AGENTS.md`, `CLAUDE.md`, skills, hooks, permissions, or harness configuration.

For development, use `cargo run -- <command>` in place of `driftctl`.

## Codex adapter

After starting a workflow, `driftctl run codex` launches the installed Codex CLI against the current unresolved frontier and records its JSONL trajectory under `.driftctl/trajectories/`. Each fresh run receives the complete unresolved goal, so continuity does not depend on a native agent session.

The adapter uses `codex exec --json --ephemeral --sandbox workspace-write`, following the [official OpenAI documentation for non-interactive Codex runs](https://developers.openai.com/codex/noninteractive/). Existing repository instructions and user configuration remain active; `driftctl` does not rewrite them. It verifies that root `AGENTS.md`, root `CLAUDE.md`, and its ledger are unchanged after the run. This is a continuity adapter, not an additional security sandbox.
