# TASK-031 — Filesystem MCP: apply_patch Schema and Rejection Paths

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-030 accepted.

## Goal

Add typed patch request parsing and safe rejection paths only.

## Read first

`TASK-000-project-rules.md`, `TASK-030-fs-patch-strategy.md`, `src/mcp/fs_server.rs`, patch design note.

## In scope

- Parse `path`, `unified_diff`, and `expected_sha256`.
- Reject malformed/oversized requests.
- Keep tool disabled or return not-enabled response until patch application is separately approved.

## Explicit non-goals

- Applying patches; activating mutation capability; shelling out to patch/git.

## Required tests / evidence

- No patch request changes a file.
- Malformed and oversized requests fail boundedly.

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
