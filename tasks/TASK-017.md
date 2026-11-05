---
# TASK-017: Add the optional read-only Git provider

**Version:** v1
**Depends on:** TASK-009, TASK-016
**Status:** Not started

## Objective
Add optional local Git status and diff inspection that remains read-only, bounded, and non-essential so the harness still works when Git is unavailable or disabled.

## Context
Build Order Phase 3, Step 10 says: "Then optionally add read-only Git status/diff provider." Section 12.3 requires direct argv invocation only; no shell; no pager; disabled external diff behavior; disabled Git prompting; disabled unsafe repository-config execution paths where supported; bounded output; and no Git mutation commands. If Git is unavailable or disabled, the application still operates using the Explorer, file reads, search, session write-log, and content hashes. Section 21, Open Question 2 leaves default enablement undecided.

## Scope
- Implement optional `git_status()` and `git_diff(staged?)` read-only actions.
- Invoke Git directly with argv only and without a shell.
- Disable pager, external diff behavior, prompting, and unsafe repository-config execution paths where supported.
- Bound all Git output.
- Keep Git non-essential and gracefully degrade when unavailable or disabled.
- Integrate optional Git state indicators with the Explorer where applicable.
- ⚠️ Source plan underspecifies this item — verify before starting. Default enablement when Git is installed is Open Question 2.

## Acceptance Criteria
- `git_status` and `git_diff` are read-only and bounded.
- Git is invoked directly with argv and without a shell.
- Pager, external diff, prompting, and unsafe repository-config execution paths are disabled where supported.
- No Git mutation command is available.
- The application works when Git is unavailable or disabled.
- Default enablement behavior is confirmed against Open Question 2 before completion.

## Out of Scope
- Git push, fetch, pull, reset, checkout, clean, rebase, merge, tag, or submodule actions.
- Git CI/CD, release publishing, or remote PR creation.
- Making Git required for Chat, Context, Session, or tool substrate.
- Whole-repository history analysis.
---
