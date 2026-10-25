# TASK-020 — Shared Project-Root and Sensitive-Path Guard

**Initial status:** Blocked  
**Depends on:** TASK-010 accepted

## Goal

Create one reusable path-policy API for orchestrator and helper scripts.

## Scope

Canonicalize the configured root; reject absolute paths, NUL bytes, traversal,
harness-private paths, sensitive patterns, and root escapes. Provide stable error
codes and APIs for existing paths and proposed new-file paths.

## Required Tests / Evidence

Unit tests for absolute paths, empty paths, NUL bytes, traversal, valid nested
paths, secret paths, audit/Wiki denial, and stable error codes.

## Acceptance Criteria

- Both pre-dispatch and script layers can use the same policy semantics.
- Denied paths are rejected before approval.
- Policy error output is bounded and safe to show.

## Non-Goals

Do not implement symlink traversal resolution beyond the focused follow-up in
TASK-021; do not implement file I/O.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
