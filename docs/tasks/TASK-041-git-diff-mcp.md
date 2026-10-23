# TASK-041 — Git MCP: diff

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-040 accepted.

## Goal

Add read-only `diff(staged: bool)` to the existing Git MCP server.

## Read first

`TASK-000-project-rules.md`, `TASK-040-git-status-mcp.md`, Git MCP source/tests.

## In scope

- Unstaged: `git -C <root> --no-pager diff`.
- Staged: `git -C <root> --no-pager diff --cached`.
- Maintain timeout/output limits and exact two-tool manifest.

## Explicit non-goals

- All Git mutation or remote operations.

## Required tests / evidence

- Staged/unstaged outputs distinct; bounded output; no Git state change.

## Required verification

```sh
cargo fmt --check
cargo check --all-targets
cargo check --all-targets --features remote
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features remote -- -D warnings
HARNESS_REQUIRE_BWRAP=1 cargo test
HARNESS_REQUIRE_BWRAP=1 cargo test --features remote
```

## Completion criteria

- Scope remains limited to this task.
- Required tests/evidence succeed.
- One focused commit is created.
- Completion report follows `TASK-000-project-rules.md`.
