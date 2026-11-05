---
# TASK-032: Enforce the Change-mode completion gate

**Version:** v2
**Depends on:** TASK-020, TASK-023, TASK-025, TASK-026, TASK-027, TASK-029, TASK-030, TASK-031
**Status:** Not started

## Objective
Wire the Change lifecycle together so completion requires an approved mutation, passing verification, bounded review, and compiled evidence rather than a model-authored completion claim.

## Context
Section 15.1 defines Change as `Learn → Plan → Act → Verify → Review → Compile` with approval required. Section 13.4 states: "A Change task cannot complete merely because the model writes `VERIFIED_DONE`." Section 15.4 states that for Change mode: approved mutation + passing verification + bounded review + evidence compilation makes the task eligible for completion. If review identifies a concrete unmet requirement, the loop returns to Act only within the configured task budget.

## Scope
- Connect the Change lifecycle to the approval, mutation, verifier, stale-detection, review, and compilation components.
- Require an approved mutation and passing verification before review.
- Require a bounded review and compiled evidence before completion.
- Prevent completion based solely on `VERIFIED_DONE`.
- Return to Act from Review only within the configured task budget when a concrete unmet requirement is found.
- Block completion when verification is stale or failed.

## Acceptance Criteria
- Change mode follows Learn → Plan → Act → Verify → Review → Compile.
- Completion requires an approved mutation, passing verification, bounded review, and compiled evidence.
- `VERIFIED_DONE` alone cannot complete a Change task.
- Review can return to Act only within the configured task budget.
- Failed or stale verification blocks completion.
- Completion state is determined by the harness, not the model.

## Out of Scope
- Implementing approval, mutation, verification, or stale detection (TASK-027 through TASK-031).
- Workflow-specific profiles (TASK-033 through TASK-037).
- Automatic Git checkpoints or resets.
- Model-controlled completion state.
---
