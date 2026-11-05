---
# TASK-014: Implement the read_file read-only tool

**Version:** v1
**Depends on:** TASK-009
**Status:** Not started

## Objective
Provide a bounded, policy-gated file reader that returns permitted UTF-8 text with hashes and truncation metadata while never exposing binary content or denied files.

## Context
Build Order Phase 3, Step 10 lists `read_file` as a read-only native tool. Section 12.2 specifies: permitted UTF-8 text only; bounded output; optional bounded line range; returns content hash and truncation metadata; does not expose binary content. Section 5.2 places `read_file` in the read-only repository profile with no mutation available.

## Scope
- Implement `read_file(path, line_start?, line_end?)` as a native read-only tool.
- Permit UTF-8 text only and reject binary content.
- Apply bounded output and optional bounded line ranges.
- Return a content hash and truncation metadata.
- Route every path check through the shared policy.
- Deny sensitive and harness-owned paths.

## Acceptance Criteria
- Only permitted UTF-8 text is returned.
- Binary content is not exposed.
- Output and line ranges are bounded.
- Results include a content hash and truncation metadata.
- Sensitive and denied paths are rejected through the shared policy.
- No mutation operation is available through this tool.

## Out of Scope
- `list_directory` and `search_text` (TASK-013, TASK-015).
- Explorer rendering (TASK-016).
- Git provider operations.
- File writes or edits.
- Unbounded full-file reads.
---
