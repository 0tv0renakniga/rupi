---
# TASK-000: Implement runtime configuration, limits, and endpoint policy

**Version:** v1
**Depends on:** None
**Status:** Done

## Objective
Establish the trusted runtime foundation so the harness can start with a validated local endpoint, conservative context limits, a harness-owned session-data path, and safe startup diagnostics.

## Context
From Build Order Phase 1, Step 1: "Implement: config parsing; endpoint validation; effective context limits; conservative output reservation; local session-data path; safe startup diagnostics." Section 6.1 defines `Config` and `Limits`. Section 6.2 sets the default endpoint to `http://127.0.0.1:8080/v1/chat/completions`, requires loopback-only acceptance by default, and prohibits automatic local-to-remote fallback. Section 6.3 places trusted operator configuration under `$XDG_CONFIG_HOME/local-agent-harness/` or `~/.config/local-agent-harness/`.

## Scope
- Define the `Config` and `Limits` models with root, LLM base, model, act/verify budgets, context cap, reserved output, preview/search/tool/session byte limits.
- Load trusted configuration from the trusted operator configuration directory.
- Validate the default endpoint against loopback-only policy and reject disallowed endpoints.
- Compute an effective context cap and conservative output reservation for later admission control.
- Resolve the harness-owned local session-data path.
- Provide safe startup diagnostics that display resolved non-secret configuration only.
- Configure the HTTP dependency without default features; keep remote support behind a separate explicit Cargo feature.

## Acceptance Criteria
- The binary starts with a valid local configuration.
- A loopback endpoint is accepted.
- A disallowed endpoint is rejected.
- Resolved non-secret configuration can be displayed.
- Context limits and reserved output are available to the runtime.
- Credentials and credential references never appear in UI, diagnostics, `Debug`, errors, or session files.

## Out of Scope
- TUI layout or rendering.
- SSE normalization, chat requests, or cancellation.
- Session JSONL persistence.
- Remote endpoint support, remote origin allowlists, or credential storage.
- Tool execution, context loading, or mutation.
---
