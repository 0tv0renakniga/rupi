# TASK-110 — Audit Permissions, Retention, and Rotation

**Initial status:** Ready  
**Depends on:** —

## Goal

Harden local audit lifecycle for repeated use.

## Scope

Set Unix defaults of 0700 directory and 0600 files where supported; add
between-task retention/rotation design; preserve append-only task evidence; update
operations documentation.

## Required Tests / Evidence

Permission and rotation-boundary tests, including audit-path denial to model
file actions.

## Acceptance Criteria

- Audit is not world-readable by default on supported systems.
- Rotation never splits one task trail ambiguously.
- Existing per-record cap remains enforced.

## Non-Goals

Do not rewrite existing evidence or rotate mid-task.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
