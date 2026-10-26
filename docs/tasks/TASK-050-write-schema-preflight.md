# TASK-050 — Write-File Schema and Policy Preflight

**Initial status:** Blocked
**Status:** Awaiting review
**Depends on:** TASK-012 and TASK-021 accepted

## Goal

Define the narrow write action and ensure unsafe requests fail before approval.

## Scope

Add strict `WriteFile { path, content, expected_sha256 }` validation, file-size
cap, UTF-8 rules, SHA-256 format validation, mode/skill checks, and preflight
integration with the existing approval gate.

## Required Tests / Evidence

Tests for malformed schemas, invalid hash, oversize content, secret path,
symlink escape, read-only mode request, and policy denial without approval UI.

## Acceptance Criteria

- No write dispatch exists yet.
- Only Change mode can reach approval preflight.
- Denied action never creates approval prompt.
- Approval metadata is bounded and redacted.

## Non-Goals

Do not create or overwrite files.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
