# TASK-041 — Plan Mode and Durable Plan Artifact

**Initial status:** Blocked
**Status:** Accepted
**Depends on:** TASK-040 accepted

## Goal

Add a no-mutation planning workflow that produces a bounded durable plan from
repository evidence.

## Scope

Implement Plan mode, tool-less/bounded planning prompt, plan review step, and
`.ai-wiki/plans/<task-id>.md` persistence. Require goal, likely files, ordered
steps, assumptions, risks, and expected verifier profile; mark unsupported
verification as a blocker rather than inventing commands.

## Required Tests / Evidence

Fake-model tests for plan artifact, read-only action restriction, invalid plan
fallback, and explicit unsupported-verifier blocker.

## Acceptance Criteria

- Plan mode exposes no mutation action.
- Artifact is bounded/redacted and durable.
- Harness retains verifier selection authority.

## Non-Goals

Do not automatically start Change mode or write files.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
