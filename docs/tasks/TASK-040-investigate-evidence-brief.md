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

## Required Tests / Evidence

End-to-end fake-model fixture repo test: inspect source and Git state, compile
brief, verify no repository mutation, test cancellation and compiler fallback.

## Acceptance Criteria

- Investigate exposes read-only actions only.
- Completion requires durable evidence brief.
- Brief is bounded/redacted and distinguishes facts from inferences.
- No mutation can be approved or dispatched in this mode.

## Non-Goals

Do not implement planning or change-mode mutation.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
