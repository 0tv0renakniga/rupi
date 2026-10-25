# TASK-033 — Read-Only Action Dispatch and Bounded Model Context

**Initial status:** Blocked  
**Depends on:** TASK-030, TASK-031, and TASK-032 accepted

## Goal

Wire fixed read-only actions through orchestrator skill dispatch and ensure
outputs entering the model context are bounded and redacted.

## Scope

Expose only `read_file`, `git_status`, and `git_diff` to read-only modes.
Map model action requests into strict Action values and script-runner calls.
Record tool/action events, bounded model feedback, and audit evidence.

## Required Tests / Evidence

Fake LLM plus real fixture-script tests for action dispatch, malformed action
teaching error, bounded result injection, cancellation, and policy denial without
approval prompt.

## Acceptance Criteria

- Read-only model catalogs contain no write/verifier/mutation action.
- Every action passes policy before dispatch.
- Model receives bounded, redacted results only.

## Non-Goals

Do not introduce Investigate artifact generation yet; do not expose writes.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
