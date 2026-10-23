# TASK-051 — Shell MCP: One Harmless argv Command

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-050 accepted.

## Goal

Implement argv-only execution of exactly one low-risk command such as `git --version` or `cargo --version`.

## Read first

`TASK-000-project-rules.md`, `TASK-050-shell-schema.md`, policy and runtime code.

## In scope

- Use Command/tokio Command only; no shell.
- Minimal environment, root cwd, bounded stdout/stderr, hard timeout, kill on timeout.

## Explicit non-goals

- General commands, cargo test, npm/pip/python/node, network, package install.

## Required tests / evidence

- Allowlisted command works; unauthorized program/cwd/argument rejects; timeout kills child; output bounds.

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
