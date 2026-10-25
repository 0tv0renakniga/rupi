# TASK-070 — Change-Mode End-to-End Disposable-Repository Pilot

**Initial status:** Blocked  
**Depends on:** TASK-040, TASK-041, and TASK-061 accepted

## Goal

Prove the complete fixed-script change workflow against a disposable local
repository using a fake or controlled local backend as appropriate.

## Scope

Exercise Learn, Plan, approval, one bounded write, verification, diff review,
Wiki compilation, and final reporting. Include one deliberate verification
failure/recovery path.

## Required Tests / Evidence

Integration test and operator runbook evidence for intended-file-only change,
approval denial, failure recovery, audit/Wiki persistence, and no MCP runtime
dependency.

## Acceptance Criteria

- One approved bounded mutation reaches Completed only after verification.
- Deliberate failing change remains non-complete until fixed.
- Evidence reconstructs action, approval, verification, and diff summary.

## Non-Goals

Do not use this as a live model/TUI validation; do not add checkpoint commits.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
