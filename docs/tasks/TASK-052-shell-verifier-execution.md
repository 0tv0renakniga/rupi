# TASK-052 — Shell MCP: Harness-Selected Verifier Commands

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-051 accepted.

## Goal

Allow only deterministic commands already selected by the verifier engine.

## Read first

`TASK-000-project-rules.md`, `TASK-051-shell-version-command.md`, verifier code/tests.

## In scope

- Integrate harness-selected Cargo/Node/Python verifier commands.
- Preserve allowlist, bounds, no-shell behavior, and fixed command ownership.

## Explicit non-goals

- Arbitrary model commands; package installation; network access.

## Required tests / evidence

- Model cannot alter verifier argv.
- Successful and timeout/error verifier cases are bounded and audited.

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
