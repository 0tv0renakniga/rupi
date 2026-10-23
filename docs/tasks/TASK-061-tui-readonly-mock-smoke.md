# TASK-061-tui-readonly-mock-smoke — TUI Read-Only Mock Smoke

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-060 accepted.

## Goal

Exercise task submission, visible streaming, cancellation, and read-only tool display using mock MCP.

## Read first

`TASK-000-project-rules.md`, relevant TUI/orchestrator/MCP tests.

## In scope

- Follow the stated goal only.

## Explicit non-goals

Do not claim production MCP validation or permit mutation.

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
