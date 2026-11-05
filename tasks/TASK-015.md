---
# TASK-015: Implement the search_text read-only tool

**Version:** v1
**Depends on:** TASK-009
**Status:** Not started

## Objective
Provide a bounded literal-substring search tool that helps the model locate relevant repository text without permitting model-supplied regular expressions or exposing denied, binary, or oversized files.

## Context
Build Order Phase 3, Step 10 lists `search_text` as a read-only native tool. Section 12.2 specifies: literal substring search only; no model-provided regular expressions; bounded scope and result count; bounded line excerpts; excludes denied paths and oversized or binary files. Section 5.2 places `search_text` in the read-only repository profile with no mutation available.

## Scope
- Implement `search_text(query, include_paths?, max_results?)` as a native read-only tool.
- Support literal substring search only.
- Reject model-provided regular expressions.
- Enforce bounded scope, result count, and line-excerpt length.
- Exclude denied paths, oversized files, and binary files.
- Route every path check through the shared policy.

## Acceptance Criteria
- Search is literal substring only.
- Model-provided regular expressions are not accepted.
- Scope, result count, and excerpts are bounded.
- Denied, oversized, and binary files are excluded.
- Results are stable and policy-compliant.
- No mutation operation is available through this tool.

## Out of Scope
- `list_directory` and `read_file` (TASK-013, TASK-014).
- Explorer rendering (TASK-016).
- Git history search.
- Regex, fuzzy, or semantic search.
- File writes or edits.
---
