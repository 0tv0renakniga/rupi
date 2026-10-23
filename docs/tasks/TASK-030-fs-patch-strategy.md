# TASK-030 — Filesystem MCP: Select Patch Strategy

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-022 accepted and explicit user approval of design required before TASK-031.

## Goal

Document and obtain approval for a safe bounded single-file unified-diff strategy before implementation.

## Read first

`TASK-000-project-rules.md`, `TASK-022-fs-write-overwrite.md`, current Cargo dependencies and security policy.

## In scope

- Evaluate maintained Rust library versus deliberately narrow parser.
- Record license, maintenance, input bounds, and why no shell-out is used.
- Produce design note only.

## Explicit non-goals

- Adding dependencies, enabling apply_patch, modifying production behavior.

## Required tests / evidence

- Design note identifies one strategy and explicit rejection cases: multi-file, binary, malformed, ambiguous, oversized, and non-applying diffs.

## Required verification

```sh
git diff --check
```

## Completion criteria

- Scope remains limited to this task.
- Required tests/evidence succeed.
- One focused commit is created.
- Completion report follows `TASK-000-project-rules.md`.
