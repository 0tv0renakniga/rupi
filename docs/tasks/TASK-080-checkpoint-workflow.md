# TASK-080 — Operator-Approved Local Checkpoint Workflow and Runbook

**Initial status:** Blocked  
**Depends on:** TASK-072 accepted

## Goal

Define and test a local Git checkpoint process after a verified microtask,
without exposing model-driven Git mutation by default.

## Scope

Add pre-checkpoint report requirements, explicit operator confirmation, commit
metadata design, untracked-file warning, rollback guidance, and disposable-repo
recovery drills. Implement commit code only if the approved design explicitly
requires it.

## Required Tests / Evidence

Runbook tests/rehearsal: verified diff review, unexpected untracked file
handling, commit metadata, and rollback limits for untracked artifacts.

## Acceptance Criteria

- Operator understands Git is not the sole security boundary.
- No auto-commit default exists.
- Commit/pilot evidence records task, microtask, verifier status, changed-file
  count, and resulting hash if a commit is created.

## Non-Goals

Do not add push/fetch/reset/checkout/clean/rebase/merge or model-initiated Git
mutation.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
