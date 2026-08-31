---
name: driftctl
description: Explicitly enable, disable, or inspect Driftctl continuity for the current Codex session when the user invokes $driftctl.
---

# Driftctl Session Control

Supported invocations:

- `$driftctl on`: enable Driftctl for this exact session.
- `$driftctl off`: disable Driftctl for this exact session.
- `$driftctl status`: report whether this exact session is enabled.
- Codex may qualify the same exact controls as `$driftctl-codex:driftctl on`, `$driftctl-codex:driftctl off`, or `$driftctl-codex:driftctl status`.

The lifecycle hook performs the control action. Report success only when developer context for this turn contains `DRIFTCTL CONTROL RESULT` or `DRIFTCTL ACTIVE INTENT`. If neither is present, say the control did not run and show `$driftctl on`, `$driftctl off`, and `$driftctl status`; never infer success from this skill. Do not run external attach, detach, or status commands, and do not ask the user for a session ID.

Never activate Driftctl implicitly. If the invocation is not exactly one of the supported forms, show those forms without changing session state.
