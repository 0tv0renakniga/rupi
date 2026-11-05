---
# TASK-027: Implement the mutation approval gate

**Version:** v2
**Depends on:** TASK-009, TASK-018, TASK-025, TASK-026
**Status:** Not started

## Objective
Add a fail-closed approval gate that validates proposed actions against schema, mode, skill, and path policy and requires an active matching operator approval before any mutation can dispatch.

## Context
Build Order Phase 4, Step 14 includes the "approval gate." Section 16.2 defines the approval order: parse action; schema validation; task-mode/skill preflight; path and policy preflight; bounded redacted approval request; active matching operator approval; dispatch; audit evidence; verification marked stale. It states: "Denied, expired, cancelled, stale, missing, or mismatched approvals never execute a mutation." Section 3.3 states that the model does not control mutation approvals.

## Scope
- Parse proposed actions and perform schema validation.
- Perform task-mode and selected-skill preflight checks.
- Perform path and policy preflight through the shared policy.
- Produce a bounded, redacted approval request.
- Require an active matching operator approval before dispatch.
- Fail closed for denied, expired, cancelled, stale, missing, or mismatched approvals.
- Record audit evidence and mark verification stale after dispatch.

## Acceptance Criteria
- Approval requests are bounded and redacted.
- The approval order matches Section 16.2.
- Only an active matching operator approval permits dispatch.
- Denied, expired, cancelled, stale, missing, or mismatched approvals never execute a mutation.
- The model cannot approve or bypass its own action.
- Dispatch records audit evidence and marks verification stale.

## Out of Scope
- Implementing `write_file` (TASK-029).
- Implementing verifier execution (TASK-030).
- Detecting external changes (TASK-031).
- Workflow-specific approval policy.
---
