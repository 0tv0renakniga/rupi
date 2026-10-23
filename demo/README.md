# Production-safe demonstration

This directory contains a small, deterministic demonstration of the harness in
its production-safe local shape. It is intentionally separate from the normal
test suite because it starts real Bubblewrap-contained MCP processes and runs
nested Cargo verification.

Run it with:

```sh
./demo/run.sh
```

The script requires Linux, `bwrap`, `cargo`, and `rustfmt`. It sets
`HARNESS_REQUIRE_BWRAP=1`, so a missing sandbox fails the run instead of
silently skipping it.

## What is demonstrated

The demo runs three tasks against fresh temporary Cargo fixtures:

1. **Read-only inspection** using the `read_only` MCP profile. The model reads
   `src/lib.rs`; no mutation or approval is performed.
2. **Add function and test** using `workspace_mutation`. The model writes a new
   function and test; the operator approval gate approves the write; Cargo
   verification passes.
3. **Verification recovery** using `workspace_mutation`. The first write leaves
   a failing test, the harness blocks completion and returns verification
   evidence, and the second write fixes it before completion.

Every task uses:

- the repository's mock MCP test binary in real-filesystem mode, not a
  third-party production MCP server;
- fixed `filesystem`, `git`, and optional `shell` MCP slots;
- canonical absolute MCP executable paths and a deny-by-default SHA-256
  allowlist;
- `bubblewrap` sandboxing with no unsandboxed override;
- `ApprovalPolicy::default()`, so mutations require approval;
- harness-owned Cargo verification (`cargo fmt --check`, `cargo check`, and
  `cargo test`);
- append-only local audit evidence and compiled Wiki memory.

The model is scripted and deterministic. This proves the harness boundary,
approval, verification, recovery, audit, and Wiki wiring; it does not measure
live model quality, exercise the HTTP model client, or certify a third-party MCP
server.

## Evidence

Each run writes to `target/demo-evidence/run-<timestamp>-<pid>/`. The directory
contains only bounded evidence, not the fixture repositories:

```text
summary.json
<task>/evidence/summary.json
<task>/evidence/audit.jsonl
```

`summary.json` records the Bubblewrap version, MCP test-binary digest, task
state, verification result, approval count, audit sizes, final file digest, and
Wiki status. Each audit line is bounded to 16 KiB, and the demo asserts that raw
source and the mock tool's secret marker do not appear in audit evidence.

Inspect a run with:

```sh
latest="$(find target/demo-evidence -mindepth 1 -maxdepth 1 -type d -name 'run-*' | sort | tail -1)"
jq . "$latest/summary.json"
jq -c '{kind,message,metadata}' "$latest/verification-recovery/evidence/audit.jsonl"
```

Retain evidence according to local policy, or remove old runs with:

```sh
rm -rf target/demo-evidence/run-*
```

## Live local-model smoke test

The demo deliberately does not depend on a model. For a live smoke test, point
the harness at a loopback OpenAI-compatible server and use a disposable
repository:

```sh
export HARNESS_ROOT=/path/to/disposable/repo
export HARNESS_ENDPOINT_PROFILE=local
export HARNESS_LLM_BASE=http://127.0.0.1:8080
export HARNESS_MODEL=local-model
export HARNESS_JEV_BASE=http://127.0.0.1:9090
export HARNESS_MCP_PROFILE=read_only
# Configure fixed MCP executables, Bubblewrap mode, and approved digests.
cargo run -- --headless
cargo run
```

Start with `read_only` and no approvals, then move to `workspace_mutation` only
after reviewing the audit output. The full operator runbook is in
[`../docs/OPERATIONS.md`](../docs/OPERATIONS.md).
