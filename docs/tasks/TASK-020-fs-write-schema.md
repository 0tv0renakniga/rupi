# TASK-020 — Filesystem MCP: write_file Schema Only

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-010 accepted.

## Goal

Add typed `write_file` request parsing and safe rejection behavior without enabling writes.

## Read first

`TASK-000-project-rules.md`, `src/mcp/fs_server.rs`, `src/mcp/capability.rs`, `src/policy.rs`, `tests/filesystem_mcp_tests.rs`.

## In scope

- Define `path`, `content`, and optional `expected_sha256`.
- Validate arguments.
- In read-only mode do not advertise `write_file`.
- A direct call must return bounded `isError: true` without touching the filesystem.

## Explicit non-goals

- Creating or overwriting files; capability activation; approval changes; patch support; Git/shell work.

## Required tests / evidence

- Invalid arguments reject safely.
- `write_file` call cannot modify a fixture.
- Existing read-only tests remain green.

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
