# TASK-063-tui-approved-mutation-pilot — TUI Approved Mutation Pilot

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-022, TASK-041, TASK-052, TASK-060 accepted.

## Goal

Run one controlled write task in disposable repository.

## Read first

`TASK-000-project-rules.md`, relevant TUI/orchestrator/MCP tests.

## In scope

- Follow the stated goal only.

## Explicit non-goals

Approval required; denial must do nothing; verification required.

## Required tests / evidence

- Capture exact local evidence and known environment limitations.

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
