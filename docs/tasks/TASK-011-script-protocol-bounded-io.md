# TASK-011 — Script Protocol, Bounded I/O, and Error Mapping

**Initial status:** Blocked  
**Depends on:** TASK-010 accepted

## Goal

Define and implement the versioned one-request/one-response JSON protocol
between the runner and fixed helper scripts.

## Scope

Add strict request/response structs, bounded stdin, stdout, and stderr
collection, response parsing, stable error codes, and structured action
outcomes. Stdout is protocol JSON only; stderr is diagnostics only.

## Required Tests / Evidence

Mock scripts must cover valid response, invalid JSON, multiple JSON responses,
nonzero exit, response-size cap, stderr cap, and missing required fields.

## Acceptance Criteria

- Oversized/invalid protocol data fails closed as bounded structured outcome.
- Error text safe for UI/model context is bounded.
- No raw stderr or full request body is blindly persisted.
- Protocol version is explicit and tested.

## Non-Goals

Do not implement real filesystem/Git/write/verifier scripts.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
