# TASK-021 — Existing-Path and New-Parent Symlink-Escape Enforcement

**Initial status:** Blocked  
**Depends on:** TASK-020 accepted

## Goal

Close path escapes through existing symlinks and symlinked parent directories
for proposed new files.

## Scope

Canonicalize existing targets and nearest existing parents; reject paths that
resolve outside the canonical root. Cover nested parent creation without allowing
an escape.

## Required Tests / Evidence

Use temporary fixtures for existing symlink escape, symlinked parent escape,
valid in-root symlink where policy permits it, and missing nested parent cases.

## Acceptance Criteria

- Existing and new-file paths cannot escape through symlinks.
- Error code clearly distinguishes path/symlink denial.
- Tests run on supported platform assumptions and skip only with documented
  platform constraints.

## Non-Goals

Do not add write operations or weaken sensitive-path policy.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
