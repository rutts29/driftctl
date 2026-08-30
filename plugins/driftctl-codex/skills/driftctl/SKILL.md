---
name: driftctl
description: Explicitly enable, disable, or inspect Driftctl continuity for the current Codex session when the user invokes $driftctl.
---

# Driftctl Session Control

Supported invocations:

- `$driftctl on`: enable Driftctl for this exact session.
- `$driftctl off`: disable Driftctl for this exact session.
- `$driftctl status`: report whether this exact session is enabled.

The lifecycle hook performs the control action and provides its result as developer context. Report that result in one short sentence. Do not run external attach, detach, or status commands, and do not ask the user for a session ID.

Never activate Driftctl implicitly. If the invocation is not exactly one of the supported forms, show those forms without changing session state.
