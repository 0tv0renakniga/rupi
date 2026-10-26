# TASK-061 — Verification Gate Integration After Mutation

**Initial status:** Blocked  
**Status:** Awaiting review
**Depends on:** TASK-052 and TASK-060 accepted

## Goal

Integrate script-backed verification as the mandatory post-mutation completion
gate for Change mode.

## Scope

Wire mutation success to stale verification state, invoke selected verifier,
return bounded failure evidence to remediation, enforce retry budget, and block
when retries/profile prerequisites are exhausted.

## Required Tests / Evidence

Orchestrator tests for verified mutation completion, failed verification return
to Act, exhausted retries blocked, no verifier blocked, and model done claim
rejected after failure.

## Acceptance Criteria

- No successful write can complete without passing required verification.
- Verification failure cannot be overridden by model or Jev.
- Audit/Wiki capture verifier outcome.

## Non-Goals

Do not implement checkpoint commits or real-model pilot in this task.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
