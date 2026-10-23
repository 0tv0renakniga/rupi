# TASK-022 — Filesystem MCP: Hash-Guarded Overwrite

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-021 accepted.

## Goal

Permit existing regular-file replacement only with a matching expected SHA-256.

## Read first

`TASK-000-project-rules.md`, `TASK-021-fs-write-new-file.md`, `src/mcp/fs_server.rs`, filesystem tests.

## In scope

- Require `expected_sha256` for existing-file overwrite.
- Compute current digest before writing.
- Preserve existing Unix mode where practical.
- Reuse atomic temp-file-plus-rename path.

## Explicit non-goals

- Patch support; symlink targets; directory replacement; binary support; permission broadening.

## Required tests / evidence

- Correct hash succeeds.
- Missing/incorrect hash fails with bytes unchanged.
- Symlink target rejects.
- Mode retained where test platform supports it.
- Audit has metadata only.

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
