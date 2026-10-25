# TASK-030 — Read-File Helper Script

**Initial status:** Blocked  
**Depends on:** TASK-012 and TASK-021 accepted

## Goal

Implement the first real fixed helper script: bounded text reading below the
configured root.

## Scope

Create `scripts/read_file.py` using the shared policy contract. Support
relative path plus optional 1-based inclusive line bounds. Read UTF-8 text only,
cap output, return truncation metadata, and emit protocol JSON only on stdout.

## Required Tests / Evidence

Fixture tests for complete read, line range, range validation, truncation,
non-UTF-8 data, sensitive file denial, path escape, and symlink escape.

## Acceptance Criteria

- Only permitted in-root UTF-8 content is returned.
- Line indexing is documented and tested.
- Output has predictable bounded shape.
- No mutation occurs.

## Non-Goals

Do not read binary data, glob directories, or expose arbitrary filesystem
metadata.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
