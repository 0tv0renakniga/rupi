# TASK-072 — Disposable Real-Repository Acceptance and Recovery Drills

**Initial status:** Blocked  
**Depends on:** TASK-071 accepted

## Goal

Run the workflow on a disposable copy of a real repository and exercise
operational failure/recovery procedures.

## Scope

Perform read-only orientation, verified bounded change, failed-verification
recovery, invalid audit path, missing script, model unavailable, unwritable
audit parent, and terminal recovery drills. Archive redacted operator evidence.

## Required Tests / Evidence

Use the operations runbook; record exact harness/model/script versions and
outcomes. Confirm no secrets/raw prompt/source payloads enter audit records.

## Acceptance Criteria

- Read-only task makes no changes.
- Bounded change affects approved scope and verifies.
- Failure drills fail closed with actionable errors.
- Evidence is sufficient for independent review.

## Non-Goals

Do not test production customer repositories or enable unreviewed network
access.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
