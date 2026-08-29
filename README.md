# driftctl

`driftctl` is a small drift-resistant workflow for long-running coding-agent tasks.

It preserves the complete goal and later steering in a durable event ledger, reconstructs the current unresolved frontier after interruption, and requires evidence before reporting verified completion.

This repository is an independent, adapted implementation of one mechanism from a broader private engineering harness. It does not require or modify that harness.

Status: the first continuity slice is working; the CLI surface is under development.
