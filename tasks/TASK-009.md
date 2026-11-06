---
# TASK-009: Implement the shared path policy substrate

**Version:** v1
**Depends on:** TASK-000
**Status:** Done

## Objective
Provide one shared filesystem policy used by every filesystem-capable operation so path traversal, symlink escapes, and sensitive-file access are denied consistently before any tool or context read occurs.

## Context
Section 12.1 states: "Every filesystem-capable operation uses one shared `Policy`." The policy must canonicalize the project root at startup; reject absolute paths; reject NUL bytes; reject traversal outside root; canonicalize existing paths; canonicalize nearest existing parents for new-file paths; reject symlink escapes; deny sensitive files and directories; deny harness-owned session/audit configuration paths; and return stable bounded error codes. Default sensitive-path denial includes `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa`, `id_ed25519`, `credentials*`, `secrets*`, `.aws/`, `.gcp/`, `.azure/`, `.ssh/`, and `.kube/`.

## Scope
- Implement a single shared `Policy` for all filesystem-capable operations.
- Canonicalize the project root at startup.
- Reject absolute paths, NUL bytes, traversal outside root, and symlink escapes.
- Canonicalize existing paths and nearest existing parents for new-file paths.
- Deny default sensitive files and directories.
- Deny harness-owned session and audit configuration paths.
- Return stable bounded error codes.
- Add policy tests for traversal, symlinks, denied paths, sensitive paths, absolute paths, NUL bytes, and new-file parent canonicalization.

## Acceptance Criteria
- All filesystem-capable operations route through the shared policy.
- Absolute paths, NUL bytes, traversal outside root, and symlink escapes are rejected.
- Sensitive-path patterns are denied by default.
- Harness-owned session and audit paths are denied.
- Error codes are stable and bounded.
- Policy tests cover the denial cases listed in Section 19.1.

## Out of Scope
- Implementing `list_directory`, `read_file`, or `search_text`.
- Git provider execution.
- Mutation or approval.
- User-configurable sensitive-path overrides.
---
