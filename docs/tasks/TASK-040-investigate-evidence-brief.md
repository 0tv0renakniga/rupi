# TASK-040 — Investigate Mode and Evidence Brief

**Initial status:** Blocked  
**Depends on:** TASK-033 accepted

## Goal

Deliver the first complete read-only workflow: investigate a repository and
persist a structured evidence brief.

## Scope

Add operator-selected `Investigate` mode, allowed state transitions, prompt
context, evidence brief compiler, and Wiki/audit integration. Briefs must state
confirmed facts, hypotheses, uncertainty, inspected files, Git summary, and
recommended next action.

Compose the active TUI and `--headless` workflows through the shared Rust-owned
script runtime: canonical path policy, audit sink, fixed `ScriptRunner`, and
`ScriptToolExecutor` injected as a `ToolExecutor`. Investigate must not require
MCP configuration, MCP startup, or an MCP executor. The frozen MCP adapter
remains reference/optional work and is never an automatic fallback.

Gate Change mode at startup/preflight and defensively in the worker while
reviewed write and verifier scripts are unavailable.

## Required Tests / Evidence

End-to-end fake-model fixture repo test: inspect source and Git state, compile
brief, verify no repository mutation, test cancellation and compiler fallback.

Also prove:

- TUI worker and headless preflight use the same script-runtime composition.
- Investigate dispatches through a script-backed executor and never constructs
  or calls the MCP executor.
- Startup succeeds with invalid or absent MCP executable/profile/digest
  configuration when the script profile is valid.
- The model-visible Investigate catalog contains only `read_file`,
  `git_status`, and `git_diff`.
- Change mode is unavailable until `write_file.py` and `run_verifier.py` exist.

## Acceptance Criteria

- Investigate exposes read-only actions only.
- Completion requires durable evidence brief.
- Brief is bounded/redacted and distinguishes facts from inferences.
- No mutation can be approved or dispatched in this mode.
- Normal TUI and headless Investigate workflows use the fixed script executor,
  not MCP, and require no MCP runtime configuration.
- Preserve approval, root/path policy, audit, cancellation, bounded context, and
  verification behavior.

## Non-Goals

Do not implement planning or change-mode mutation. Do not expand MCP or add an
MCP fallback. Do not move runtime executor integration to release packaging.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
