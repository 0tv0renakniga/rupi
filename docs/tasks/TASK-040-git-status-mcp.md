# TASK-040 — Git MCP: status Only

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-010 accepted.

## Goal

Create a fixed-root read-only Git MCP binary advertising only `status`.

## Read first

`TASK-000-project-rules.md`, MCP runtime/config/capability code, existing mock Git tests.

## In scope

- Implement MCP handshake and one tool.
- Run only `git -C <root> --no-pager status --porcelain=v1` with minimal environment and `GIT_OPTIONAL_LOCKS=0`.
- Bound output and timeout.

## Explicit non-goals

- diff; any Git mutation; remote operations; hooks/config edits; network.

## Required tests / evidence

- Works in temp Git repo; fails outside one; exact one-tool manifest; Bubblewrap E2E; no repository state change.

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
