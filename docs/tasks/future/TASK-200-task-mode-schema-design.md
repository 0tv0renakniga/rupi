# TASK-200-task-mode-schema-design — Task Mode Schema Design

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

Real-server pilots accepted.

## Goal

Write task-mode schema/design only.

## Read first

`TASK-000-project-rules.md`, `AGENT_ORCHESTRATION_ARCHITECTURE.md`, relevant orchestrator/TUI tests.

## In scope

- Follow stated task scope only.

## Explicit non-goals

No runtime mode framework.

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
