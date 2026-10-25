# TASK-012 — Script Environment, Timeout, Cancellation, and Audit Integration

**Initial status:** Blocked  
**Depends on:** TASK-011 accepted

## Goal

Harden script execution lifecycle and make action outcomes auditable.

## Scope

Use a minimal environment; remove proxy, cloud-credential, and model-
credential variables. Enforce timeout, cancellation, kill-on-drop/process cleanup,
duration measurement, and bounded audit metadata including script identity and
action result classification.

## Required Tests / Evidence

Test timeout, cancellation, child cleanup, filtered environment, bounded audit
record, and successful action provenance. Verify no credential reference/value
appears in Debug, errors, UI, or audit evidence.

## Acceptance Criteria

- Timed-out/cancelled scripts are terminated and cannot report success.
- Minimal environment is documented and tested.
- Audit includes action ID, fixed script identity, duration, and redacted result.
- Existing audit record limits remain enforced.

## Non-Goals

Do not add OS sandbox changes, network features, or new action authority.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
