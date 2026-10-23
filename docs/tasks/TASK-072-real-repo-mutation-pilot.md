# TASK-072-real-repo-mutation-pilot — Real Repository Single Mutation Pilot

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-071 and TASK-063 accepted.

## Goal

Make one reversible approved change and verify it.

## Read first

`TASK-000-project-rules.md`, `docs/OPERATIONS.md`, relevant runtime tests.

## In scope

- Follow one documented drill only.

## Explicit non-goals

Only intended file changes; inspect diff/audit.

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
