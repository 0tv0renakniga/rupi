---
# TASK-024: Add v1 adversarial security tests

**Version:** v1
**Depends on:** TASK-010, TASK-011, TASK-013, TASK-014, TASK-015, TASK-016, TASK-018, TASK-021, TASK-022, TASK-023
**Status:** Not started

## Objective
Verify that context, policy, slash parsing, native read-only tools, Explorer, skills, and read-only modes fail closed against adversarial repository and input content.

## Context
Section 19.3 requires testing adversarial cases including hostile project `AGENTS.md`, closing-tag injection attempts, path traversal, symlink escape, secret-file access, malformed tool requests, and oversized output. Section 20.2 requires read-only tools to obey path/sensitive-file policy, outputs to be bounded, read-only skills to produce durable evidence, the optional Git provider to remain read-only and non-essential, and no mutation action to be exposed in read-only modes. This task is split from Section 19.3: mutation, approval, verifier, and external-change cases are covered by TASK-038.

## Scope
- Add tests for hostile project `AGENTS.md` and closing-tag injection.
- Add tests for path traversal, absolute paths, NUL bytes, symlink escape, and secret-file access.
- Add tests for malformed tool requests and oversized tool output.
- Verify read-only modes expose no mutation action.
- Verify the optional Git provider remains read-only and non-essential.
- Verify read-only skills only produce bounded evidence artifacts.

## Acceptance Criteria
- Hostile project instructions cannot override policy or authority.
- Closing-tag injection attempts are neutralized.
- Traversal, symlink escape, and secret-file access are denied.
- Malformed tool requests and oversized output fail closed and remain bounded.
- Read-only modes expose no mutation action.
- Git remains read-only and non-essential.
- All tests pass in the ordinary automated suite without a live model.

## Out of Scope
- Missing scripts, script hash mismatch, approval bypass, verifier failure, and external file change after verification (TASK-038).
- Mutation or approval implementation.
- Live-model-only security checks.
---
