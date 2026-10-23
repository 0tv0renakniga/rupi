# TASK-100-cli-help — CLI Help

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

—

## Goal

Add --help without starting MCP servers.

## Read first

`TASK-000-project-rules.md`, relevant source/tests/docs.

## In scope

- Follow stated task scope only.

## Explicit non-goals

No MCP/config startup.

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
