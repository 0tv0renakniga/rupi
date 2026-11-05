---
# TASK-038: Add v2 adversarial security tests

**Version:** v2
**Depends on:** TASK-027, TASK-028, TASK-029, TASK-030, TASK-031, TASK-032, TASK-033
**Status:** Not started

## Objective
Verify that the mutation, approval, verifier, stale-detection, and workflow layers fail closed against adversarial inputs and cannot be bypassed by model claims or profile definitions.

## Context
Section 19.3 requires testing adversarial cases including missing scripts, script hash mismatch, approval bypass attempts, verifier failure, and external file change after verification. Section 20.3 requires mutation to require matching approval, existing-file overwrite to require expected SHA-256, verification to be harness-selected and argv-only, failed verification to block completion, external relevant edits to invalidate prior verification, audit and Wiki artifacts to be bounded and redacted, and workflow profiles not to expand policy authority. This task is split from Section 19.3: context, policy, read-only tool, and Explorer cases are covered by TASK-024.

## Scope
- Add tests for missing trusted scripts and script SHA-256 mismatches.
- Add tests for approval bypass, denial, expiry, cancellation, stale approval, and mismatched approval.
- Add tests for verifier failure and missing dependencies.
- Add tests for external file changes after verification.
- Add tests confirming workflow profiles cannot expand policy authority.
- Add tests confirming audit and Wiki artifacts remain bounded and redacted.

## Acceptance Criteria
- Missing scripts and hash mismatches fail closed.
- Denied, expired, cancelled, stale, missing, and mismatched approvals never execute a mutation.
- Verifier failure and missing dependencies block completion.
- External relevant edits invalidate prior verification.
- Workflow profiles cannot expand policy authority.
- Audit and Wiki artifacts remain bounded and redacted.
- All tests pass in the ordinary automated suite without a live model.

## Out of Scope
- Context, path, read-only tool, and Explorer security cases (TASK-024).
- Fuzzing or sandbox implementation.
- Live-model-only security checks.
- Automatic dependency installation.
---
