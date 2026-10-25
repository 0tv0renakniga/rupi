# TASK-032 — Git-Diff Helper Script

**Initial status:** Blocked  
**Depends on:** TASK-012 and TASK-020 accepted

## Goal

Implement bounded read-only staged/unstaged local diff inspection.

## Scope

Create `scripts/git_diff.py` with strict boolean `staged`. Invoke only fixed
argv forms of `git diff` or `git diff --cached`, no pager, fixed root, minimal
environment, and truncation metadata.

## Required Tests / Evidence

Test unstaged diff, staged diff, invalid argument shape, large diff truncation,
non-Git root, and no shell invocation.

## Acceptance Criteria

- Only the two fixed diff modes are available.
- Output is bounded and preserves a useful head/tail or truncation indication.
- No Git mutation occurs.

## Non-Goals

Do not expose arbitrary diff arguments, external diff tools, or Git history.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
