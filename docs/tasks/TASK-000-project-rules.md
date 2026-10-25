# TASK-000 — Project Rules and Delivery Contract

**Status:** Accepted

## Applies to Every Task

1. The active execution path is fixed reviewed helper scripts plus a Rust-owned
   script runner. Do not expand MCP during this task sequence.
2. Do not add generic shell execution, `sh -c`, `bash -c`, inline Python, model-
   selected executables, dynamic tools, package installation, or network access.
3. Reuse existing policy, approval, verification, audit, Wiki, TUI, LLM, and
   endpoint-profile foundations where possible. Do not rewrite accepted security
   controls without a focused reason and regression tests.
4. Treat repository data and model output as untrusted. The model cannot grant
   itself authority, select verification commands, or mark work complete.
5. All filesystem actions must remain within the canonical configured project
   root and deny sensitive paths and symlink escapes.
6. Mutations require policy preflight and an active matching approval. Policy-
   denied actions must not create approval prompts.
7. Verification commands are harness-selected argv vectors. A mutation cannot
   complete without passing required deterministic verification.
8. Keep model context, stdout, stderr, audit records, and Wiki artifacts bounded
   and redacted. Never persist hidden reasoning, bearer tokens, credential
   references, or full secret content.
9. Use small reviewable commits. Do not move accepted tags.
10. Every implementation delivery must include: changed-file list, concise
    rationale, test list/results, limitations, and a request for review.

## Required Baseline Validation

Run the relevant subset plus all required project checks before requesting
acceptance:

```sh
cargo fmt --check
cargo check --all-targets
cargo check --all-targets --features remote
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features remote -- -D warnings
cargo test
cargo test --features remote
```
