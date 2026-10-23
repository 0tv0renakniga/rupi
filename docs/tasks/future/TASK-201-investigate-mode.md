# TASK-201-investigate-mode — Static Investigate Mode

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-200 accepted.

## Goal

Add static read-only Investigate mode.

## Read first

`TASK-000-project-rules.md`, `AGENT_ORCHESTRATION_ARCHITECTURE.md`, relevant orchestrator/TUI tests.

## In scope

- Follow stated task scope only.

## Explicit non-goals

No mutation; evidence brief output.

## Required tests / evidence

- Record selected mode/prompt version in local evidence if implemented.

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
