---
# TASK-013: Implement the list_directory read-only tool

**Version:** v1
**Depends on:** TASK-009
**Status:** Not started

## Objective
Provide a bounded, policy-gated directory listing tool so the model and Explorer can inspect repository layout without exposing denied paths or unbounded output.

## Context
Build Order Phase 3, Step 10 lists `list_directory` as the first read-only native tool. Section 12.2 defines it with bounded depth and entry count, hidden denied paths, optional `.gitignore` display convenience, and the rule that `.gitignore` does not override sensitive-path denial. Section 5.2 places `list_directory` in the read-only repository profile with no mutation available.

## Scope
- Implement `list_directory(path?)` as a native read-only tool.
- Enforce bounded depth and bounded entry count.
- Hide denied and sensitive paths from results.
- Optionally respect `.gitignore` for display convenience without weakening sensitive-path denial.
- Route every path check through the shared policy.
- Bound tool output and return stable errors.

## Acceptance Criteria
- `list_directory` lists only paths permitted by policy.
- Depth and entry count are bounded.
- Denied and sensitive paths are hidden.
- `.gitignore` does not override sensitive-path denial.
- Tool output is bounded.
- No mutation operation is available through this tool.

## Out of Scope
- `read_file` and `search_text` (TASK-014, TASK-015).
- Explorer rendering (TASK-016).
- Git status or diff.
- Recursive unbounded traversal.
- Write, rename, delete, or execute operations.
---
