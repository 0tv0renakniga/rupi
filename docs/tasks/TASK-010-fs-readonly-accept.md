# TASK-010 — Finalize Read-Only filesystem_mcp

## Status

Blocked until explicitly assigned and all dependencies are accepted.

## Goal

Run fresh full verification and prepare the existing read-only filesystem MCP milestone for acceptance. No new capability may be added.

## Read first

`TASK-000-project-rules.md`, `src/mcp/fs_server.rs`, `src/bin/filesystem_mcp.rs`, `tests/filesystem_mcp_tests.rs`, `REMAINING_TASKS_INDEX.md`.

## In scope

- Confirm commits `062ed2a` and `f4999c8` are present.
- Run full verification from the actual current tree.
- Confirm committed parent-directory symlink coverage, 1 MiB inbound request-line bound, exact `read_file` advertisement, and real Bubblewrap E2E.
- Correct task documentation only if it disagrees with Git/repository reality.

## Explicit non-goals

- `write_file`, `apply_patch`, Git MCP, shell MCP, dependency changes, policy changes, or runtime feature work.

## Required tests / evidence

- Real binary starts under required Bubblewrap test.
- Server cannot write, spawn subprocesses, or use network in production code.
- Fresh exact test totals are reported; ignored/skipped tests are identified.

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
