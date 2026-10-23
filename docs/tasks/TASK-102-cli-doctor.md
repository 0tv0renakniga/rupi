# TASK-102-cli-doctor — CLI Doctor

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-101 accepted.

## Goal

Add safe preflight reporting for config, MCP startup, audit path, and model reachability.

## Read first

`TASK-000-project-rules.md`, relevant source/tests/docs.

## In scope

- Follow stated task scope only.

## Explicit non-goals

No TUI, task, or mutation.

## Required tests / evidence

- Add direct tests and documentation as needed.

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
