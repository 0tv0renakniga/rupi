# TASK-051 — New-File Creation with Approval and Atomic Write

**Initial status:** Blocked  
**Depends on:** TASK-050 accepted

## Goal

Implement approved creation of a previously nonexistent UTF-8 text file.

## Scope

Create `scripts/write_file.py` new-file behavior only. Require matching active
approval; validate parent; write temporary sibling; atomically rename; clean up
on error; record redacted audit evidence; invalidate verification after success.

## Required Tests / Evidence

Tests for approved creation, denied/expired/stale/cancelled approval, existing
target rejection, nested permitted parent, temp cleanup, file-size cap, and
cancellation before dispatch.

## Acceptance Criteria

- Exactly one new permitted file is created after approval.
- Existing targets are never overwritten.
- Failed write leaves no partial target/temp artifact.
- Successful mutation requires later verification.

## Non-Goals

Do not overwrite, patch, commit, or create arbitrary directories outside
allowed parent creation.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
