# TASK-052 — Guarded Overwrite with Expected SHA-256

**Initial status:** Blocked
**Status:** Accepted
**Depends on:** TASK-051 accepted

## Goal

Allow controlled replacement of an existing permitted text file only when its
current content matches the request’s expected SHA-256.

## Scope

Extend `write_file.py` to compute current target SHA-256 immediately before
atomic replacement. Require a valid expected digest for every overwrite and
preserve file mode where practical.

## Required Tests / Evidence

Tests for matching digest success, mismatch preserving original bytes, target
changed between preflight and dispatch, sensitive/escape denial, and temp
cleanup.

## Acceptance Criteria

- No overwrite without an expected digest.
- Digest mismatch causes no target modification.
- Audit stores digest metadata, not full content.
- Existing approval rules still apply.

## Non-Goals

Do not implement patching or multi-file writes.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
