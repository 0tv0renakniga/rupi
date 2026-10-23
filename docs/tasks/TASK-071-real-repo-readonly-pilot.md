# TASK-071-real-repo-readonly-pilot — Real Repository Read-Only Pilot

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-070 accepted.

## Goal

Run an Investigate-style orientation task against disposable repository.

## Read first

`TASK-000-project-rules.md`, `docs/OPERATIONS.md`, relevant runtime tests.

## In scope

- Follow one documented drill only.

## Explicit non-goals

No mutation; audit must be bounded/redacted.

## Required tests / evidence

- Preserve bounded operator evidence.

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
