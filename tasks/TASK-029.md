---
# TASK-029: Implement controlled write_file mutation

**Version:** v2
**Depends on:** TASK-009, TASK-026, TASK-027
**Status:** Not started

## Objective
Add the narrowly scoped `write_file` mutation so approved changes can be written safely with expected-content hashes, atomic replacement, and policy enforcement.

## Context
Build Order Phase 4, Step 14 lists `write_file`. Section 16.1 requires a controlled file write to include path, content, and `expected_sha256` for existing targets. New files may omit `expected_sha256`; existing files require it; a mismatch rejects the write. Writes use a temporary sibling file plus atomic rename, clean temporary artifacts after error, enforce bounded content size, and always apply path and sensitive-file policy. Section 5.4 places `write_file` in the Controlled Change profile and requires policy preflight, approval, evidence recording, and successful verification before completion.

## Scope
- Implement `write_file` with path, content, and optional `expected_sha256`.
- Require `expected_sha256` for existing files and reject mismatches.
- Write through a temporary sibling file followed by atomic rename.
- Clean temporary artifacts after errors.
- Enforce bounded content size.
- Apply path and sensitive-file policy.
- Require the approval gate before dispatch.
- Record audit evidence and mark prior verification stale after a successful write.

## Acceptance Criteria
- New files can be written without `expected_sha256`.
- Existing files require a matching `expected_sha256`.
- Hash mismatches reject the write.
- Writes use temporary sibling files and atomic rename.
- Temporary artifacts are cleaned after errors.
- Content size is bounded.
- Policy and approval checks occur before dispatch.
- Audit evidence is recorded and prior verification is marked stale.

## Out of Scope
- Approval UI and policy (TASK-027).
- Verifier execution (TASK-030).
- Stale-change detection across later edits (TASK-031).
- Arbitrary file operations such as rename, delete, or execute.
- Multi-file patch application.
---
