# TASK-010 — Fixed Rust Script-Runner Foundation

**Initial status:** Ready  
**Depends on:** —

## Goal

Implement the Rust-owned runner that maps a strict internal action identifier
to a canonical harness-owned helper script. The model must never control an
executable, script path, interpreter, working directory, or shell command.

## Scope

Create `src/script_runner/` with action mapping and child-process launch.
Use argv-only execution, fixed reviewed interpreter policy, canonical script
paths, configured project-root working directory, and child cleanup. Define
only the initial fixed action IDs: `ReadFile`, `GitStatus`, `GitDiff`,
`WriteFile`, and `RunVerifier`. Do not yet implement the scripts.

## Required Tests / Evidence

Add mock-script tests proving: known action starts the mapped script; unknown
action is rejected; model-provided paths cannot influence launch; missing script
fails safely; no shell executable is used. Run formatting, checks, Clippy, and
focused tests.

## Acceptance Criteria

- Runner accepts only fixed internal action IDs.
- Mapping is Rust-owned and canonicalized.
- No `sh -c`, `bash -c`, or inline-code execution exists.
- Child working directory is fixed below the configured project root.
- The design leaves bounded protocol handling to TASK-011.

## Non-Goals

Do not add helper-script behavior, generic command execution, MCP expansion,
or mutation support.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
