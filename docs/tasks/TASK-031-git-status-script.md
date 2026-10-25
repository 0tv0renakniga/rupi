# TASK-031 — Git-Status Helper Script

**Initial status:** Blocked  
**Depends on:** TASK-012 and TASK-020 accepted

## Goal

Implement bounded read-only Git status inspection.

## Scope

Create `scripts/git_status.py`. Invoke fixed argv-only Git with the configured
root, no pager, `GIT_OPTIONAL_LOCKS=0`, and a minimal environment. Return
bounded display-safe status and clear non-repository errors.

## Required Tests / Evidence

Test clean and modified temporary repositories, untracked files, non-Git root,
no-pager environment, output cap, and no shell invocation.

## Acceptance Criteria

- Script performs only status inspection.
- It does not mutate index, worktree, remote, or Git configuration.
- Output is bounded and suitable for evidence briefs.

## Non-Goals

Do not implement commit, branch changes, remote operations, or generic Git
commands.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
