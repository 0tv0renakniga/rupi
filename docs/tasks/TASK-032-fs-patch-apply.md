# TASK-032 — Filesystem MCP: Apply One Bounded Single-File Patch

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Dependencies

TASK-031 accepted and explicit user approval required.

## Goal

Implement the approved strategy for one regular text file through the guarded atomic write path.

## Read first

`TASK-000-project-rules.md`, `TASK-031-fs-patch-schema.md`, approved patch strategy, `src/mcp/fs_server.rs`.

## In scope

- Require matching expected hash.
- Reject multi-file, binary, malformed, ambiguous, oversized, and non-applying diffs.
- Bound memory and output.
- Apply via guarded atomic write.

## Explicit non-goals

- Shelling out to patch/git; multi-file edits; Git operations.

## Required tests / evidence

- Valid single-file patch works.
- All invalid cases leave bytes unchanged.
- Bubblewrap E2E passes.

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
