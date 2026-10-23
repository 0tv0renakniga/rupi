# TASK-021 — Filesystem MCP: Create New File Only

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-020 accepted.

## Goal

Enable `write_file` in the workspace-mutation profile only for creating a new UTF-8 text file beneath the fixed root.

## Read first

`TASK-000-project-rules.md`, `TASK-020-fs-write-schema.md`, `src/mcp/fs_server.rs`, `src/policy.rs`, approval/orchestrator tests.

## In scope

- Repository-relative target only.
- Reject absolute paths, `..`, NUL, sensitive paths, existing targets, and any symlink escape.
- Parent creation remains under root.
- Use temp file in approved parent then atomic rename.
- Return bounded metadata: relative path, bytes, resulting SHA-256.

## Explicit non-goals

- Existing-file overwrite; apply_patch; Git/shell functionality; changes to approval defaults.

## Required tests / evidence

- New-file creation works.
- Existing target fails unchanged.
- Traversal/symlink/sensitive-path tests.
- Induced write failure cleans temporary artifacts.
- Bubblewrap E2E.
- Denied/expired approval never reaches server.

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
