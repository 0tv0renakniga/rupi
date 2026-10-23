# TASK-050 — Shell MCP: run Schema and Validation Only

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-010 accepted.

## Goal

Define `run` parsing and validation without launching any process.

## Read first

`TASK-000-project-rules.md`, policy code, verifier code, MCP runtime/capability code.

## In scope

- Parse program, args, optional cwd, optional timeout.
- Validate bare allowlisted program, root-contained cwd, timeout range, and no forbidden shell metacharacters.
- Return disabled/unsupported response.

## Explicit non-goals

- Process execution; allowlist changes; package installation; network.

## Required tests / evidence

- Invalid program/cwd/args/timeout reject; no process starts.

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
