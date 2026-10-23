# Remaining Tasks

This is the working backlog for turning the accepted harness baseline into a
usable, repeatable local deployment. It is intentionally more detailed than the
high-level roadmap in `harness_plan_v2.md`.

Baseline at the time this document was written:

- Commit: `3b5f01d Add production-safe stability demo and audit evidence`
- Tag: `stability-demo-review`
- Working tree: clean before this planning document was added
- Tests: 252 default / 251 remote passing, 2 intentionally ignored
- Demo: passing under Bubblewrap with 68 KiB of bounded evidence
- Debug binary: `target/debug/local-agent-harness`
- No release binary or install package yet
- No production MCP server binaries in the repository; the demo uses
  `tests/support/mock_mcp_server.rs`
- This document is a planning artifact and is not part of the accepted tag until
  it is explicitly committed.

Progress after this baseline:

- The read-only `filesystem_mcp` milestone is implemented and pending
  acceptance. It lives in `src/mcp/fs_server.rs`,
  `src/bin/filesystem_mcp.rs`, and `tests/filesystem_mcp_tests.rs`.
- Follow-up hardening adds committed parent-directory symlink-escape coverage
  and a 1 MiB bound on inbound JSON-RPC request lines.
- Default suite: 266 passing / remote: 265 passing, 2 intentionally ignored.
- P0.1 still needs `write_file`/`apply_patch`, `git_mcp`, and `shell_mcp`.

## What exists

| Area | State | Evidence |
| --- | --- | --- |
| Single-task orchestrator | Implemented | `src/orchestrator.rs`, `src/state_machine.rs`; Learn/Plan/Act/Verify/Review/Compile/Completed loop |
| TUI | Implemented | `src/tui/`; Ratatui/crossterm, task entry, streaming, approvals, cancellation, help, detail pane |
| Headless preflight | Implemented | `src/main.rs` `--headless`; validates config, starts/servers, lists warnings, shuts down |
| Local LLM client | Implemented | `src/llm/openai_compat.rs`; loopback HTTP, streaming, bounds, redaction |
| Jev/System 1 client | Implemented, optional | `src/jev/`; advisory routing, loopback-only |
| MCP runtime | Implemented | `src/mcp/`; fixed `filesystem`/`git`/`shell` slots, JSON-RPC, bounded stderr, fail-closed startup |
| Capability manifest | Implemented | `src/mcp/capability.rs`; exact routes: `read_file`, `write_file`, `apply_patch`, `run_command`, `git_status`, `git_diff` |
| MCP security | Implemented | Canonical executable paths, SHA-256 allowlist, Bubblewrap, no unsandboxed by default |
| Policy | Implemented | `src/policy.rs`; repository-relative paths, sensitive paths, symlink escape checks, program allowlist, no shell strings |
| Approval gate | Implemented | `src/approval.rs`, TUI prompt flow; mutation approval, timeout, denial, stale ID handling |
| Verification | Implemented | `src/agent/verify.rs`; Cargo, npm scripts, pytest detection; bounded output; Cargo/Node/Python support |
| Audit | Implemented | `src/audit.rs`; append-only JSONL, 16 KiB record cap, safe path validation, TUI wiring |
| Wiki memory | Implemented | `src/wiki/`; compiled `.ai-wiki/`, redaction, session records |
| Production-safe demo | Implemented | `tests/demo_tests.rs`, `demo/run.sh`, `demo/README.md`; 3 tasks, real Bubblewrap MCP test server, real Cargo verification |
| Operator runbook | Implemented | `docs/OPERATIONS.md`; setup, audit, recovery, rollback, known limits |
| CI | Partially verified | `.github/workflows/ci.yml` has default, remote, and required Bubblewrap lanes plus demo/evidence upload; remote CI run not yet observed from this workspace |
| Accepted tags | Immutable | `pre-endpoint-sandbox`, `m12-endpoint-sandbox-review`, `operational-hardening-review`, `stability-demo-review` |

## What remains

Priority key:

- **P0**: required before calling the local deployment usable.
- **P1**: required for repeatable installation and handoff.
- **P2**: hardening/usability; can follow the first real pilot.
- **P3**: explicitly deferred features; do not start without a new decision.

| ID | Remaining outcome | Priority | Depends on |
| --- | --- | --- | --- |
| P0.1 | Real production MCP servers for filesystem, Git, and shell | P0 | None |
| P0.2 | Live local-model + TUI smoke test | P0 | P0.1 for tools; text test can start now |
| P0.3 | Real-repository acceptance pilot, including recovery drills | P0 | P0.1, P0.2 |
| P1.1 | Release build, install layout, checksums, launcher/config template | P1 | P0.1 |
| P1.2 | CI/release automation and first remote CI green run | P1 | P1.1 |
| P2.1 | TUI/CLI first-run usability (`--help`, `--version`, config summary) | P2 | None |
| P2.2 | Audit retention, rotation, and restrictive file permissions | P2 | None |
| P2.3 | Verifier coverage for additional ecosystems or explicit verifier config | P2 | P0.3 findings |
| P3 | Dynamic MCP discovery, Git mutation, package installation, generic network, work-API adapters | P3 | New product decision |

---

## P0.1: Implement or vendor production MCP servers

### Goal

Provide reviewed, installable MCP executables for the three fixed slots so the
TUI can perform real repository work without the test mock.

### Why this is blocking

The repository currently contains only
`tests/support/mock_mcp_server.rs`. It is a test binary, not a production
filesystem/Git/shell implementation. The demo uses it with
`--real-filesystem`, but `run`, `status`, and `diff` are still mock behavior.

The harness requires exact server names, tool names, and argument shapes.
Servers that advertise extra tools fail startup. Off-the-shelf MCP servers will
usually need an adapter or a thin in-repo implementation.

### Required interface

The active profile determines which routes are required:

| Slot | Profile | Required advertised tools |
| --- | --- | --- |
| `filesystem` | `read_only` | `read_file` |
| `filesystem` | `workspace_mutation` | `read_file`, `write_file`, `apply_patch` |
| `git` | both | `status`, `diff` |
| `shell` | `workspace_mutation` | `run` |

Argument contract expected by the harness:

```text
filesystem.read_file:
  path: string
  line_start?: u32
  line_end?: u32

filesystem.write_file:
  path: string
  content: string
  expected_sha256?: string

filesystem.apply_patch:
  path: string
  unified_diff: string
  expected_sha256?: string

git.status:
  {}

git.diff:
  staged: bool

shell.run:
  program: string
  args: string[]
  cwd?: string
  timeout_secs?: u64
```

Server rules:

- Speak newline-delimited JSON-RPC 2.0 over stdin/stdout.
- Implement `initialize`, `notifications/initialized`, `tools/list`, and
  `tools/call`.
- Set a meaningful `serverInfo.name` and `serverInfo.version`; the version is
  recorded in local audit metadata.
- Do not advertise tools outside the exact routes above.
- Use a fixed root supplied by configuration, never a model-supplied root.
- Do not inherit the harness environment.
- Never invoke a shell for `run`; execute the program with argv directly.
- Bound stdout/stderr and request durations.
- Return MCP `isError: true` for denied or failed operations.

### Detailed steps

1. **Choose the implementation strategy.**
   - Recommended: add three small in-repo binaries under `src/bin/`:
     `filesystem_mcp.rs`, `git_mcp.rs`, and `shell_mcp.rs`.
   - Share the JSON-RPC loop and bounds in a library module such as
     `src/mcp/server_support.rs`.
   - Alternative: vendor reviewed third-party servers and add thin adapters that
     expose exactly the required names. This has a larger supply-chain surface.
   - Do not use model-selected executables, PATH lookup, or dynamic discovery.

2. **Implement the filesystem server.**
   - Accept a fixed `--root <path>` argument or use the configured working
     directory; canonicalize it at startup.
   - Reject absolute paths, `..` traversal, NUL bytes, and paths that resolve
     outside the root.
   - Reject symlink escapes at the server layer even though `Policy` also checks
     them.
   - Implement `read_file` with optional line bounds and bounded output. Define
     and document the indexing convention (prefer 1-based, inclusive) and test
     it explicitly.
   - Implement `write_file` with parent creation inside the root, atomic
     temp-file-plus-rename behavior, and optional `expected_sha256` conflict
     checking.
   - Implement `apply_patch` against one repository-relative file. Use a vetted
     diff implementation or a small reviewed parser; do not shell out to
     `patch`. Enforce `expected_sha256` before applying.
   - Preserve the existing file mode where practical.
   - Add a read-only mode that advertises only `read_file` if desired.

3. **Implement the Git server.**
   - Accept the same fixed root.
   - `status`: run Git with argv, no shell, no pager, and no optional locks.
     Prefer `git -C <root> --no-pager status --porcelain=v1`.
   - `diff`: run `git -C <root> --no-pager diff` or
     `git -C <root> --no-pager diff --cached` when `staged` is true.
   - Set `GIT_OPTIONAL_LOCKS=0` and a minimal environment.
   - Never implement commit, push, fetch, reset, checkout, clean, or submodule
     mutation.
   - Bound output and return clear errors for non-repositories.

4. **Implement the shell server.**
   - Accept the same fixed root.
   - `run`: validate `program` is a bare name, validate `cwd` stays inside the
     root, and enforce the same allowlist as `Policy`:
     `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `git`, `npm`, `node`,
     `python3`, `pytest`, `pip`.
   - Reject shell metacharacters in arguments, matching `Policy`.
   - Execute with `std::process::Command` or `tokio::process::Command` directly;
     never use `sh -c`.
   - Clear the environment except a minimal `PATH`, `HOME`, and required locale
     variables.
   - Enforce `timeout_secs` with a hard maximum of 900 seconds.
   - Capture bounded stdout/stderr and kill the child on timeout.
   - Do not allow `sudo`, network tools, package managers outside the allowlist,
     or arbitrary binaries.

5. **Add protocol and security tests.**
   - Startup handshake, `tools/list`, and `tools/call` for each route.
   - Exact advertised-tool set; extra tools must fail harness startup.
   - Path traversal, absolute path, symlink escape, and sensitive-path attempts.
   - `expected_sha256` mismatch must fail without writing.
   - `apply_patch` must fail cleanly on malformed or non-applying diffs.
   - Git status/diff must work in a real temporary repository and fail outside
     one.
   - Shell allowlist, metacharacter rejection, cwd escape, timeout, and output
     bounds.
   - No raw source or secrets in audit tool records.

6. **Add end-to-end coverage under Bubblewrap.**
   - Extend or add an ignored integration test that uses the real production
     servers, not only the mock.
   - Run `read_only` inspection and `workspace_mutation` add-function/fix-test
     tasks against temporary fixtures.
   - Assert capability-manifest validation, digest pinning, audit provenance,
     real Cargo verification, and Wiki compilation.
   - Keep the deterministic mock demo as the fast CI smoke test.

7. **Build and pin the binaries.**
   - Build with `cargo build --release --locked --bins`.
   - Install to a fixed reviewed location, for example `/opt/harness/bin/`.
   - Compute digests:

     ```sh
     sha256sum /opt/harness/bin/filesystem_mcp \
       /opt/harness/bin/git_mcp \
       /opt/harness/bin/shell_mcp
     ```

   - Put those values in `HARNESS_MCP_APPROVED_DIGESTS`.
   - Record the source commit, build command, Rust version, and digests in the
     deployment notes.

8. **Validate startup with the real servers.**
   - Set the three `_EXECUTABLE` variables to absolute paths.
   - Set `_SANDBOX_MODE=bubblewrap` and `_SANDBOX_EXECUTABLE=/usr/bin/bwrap`.
   - Set `HARNESS_MCP_PROFILE=read_only` first.
   - Run:

     ```sh
     cargo run -- --headless
     ```

   - Repeat with `workspace_mutation` and the shell server enabled.
   - A missing tool, extra tool, bad digest, missing Bubblewrap, or missing
     executable must fail startup with no task accepted.

### Acceptance criteria

- `--headless` succeeds with real servers for `read_only` and
  `workspace_mutation`.
- A read-only task can read a real repository file and return a useful answer.
- An approved write changes exactly one file and verification passes.
- A deliberately failing change blocks completion until fixed.
- Extra advertised tools and digest mismatches fail startup.
- No shell command string is accepted.
- No server can escape the repository root through paths or symlinks.
- Audit provenance contains the real executable path, digest, and version.
- The real-server E2E test passes under `HARNESS_REQUIRE_BWRAP=1`.

### Risks and notes

- Implementing `apply_patch` correctly is the highest-risk part; prefer a vetted
  diff library over ad hoc parsing.
- `git status` can try to refresh the index; use `GIT_OPTIONAL_LOCKS=0` and
  verify behavior in the read-only Bubblewrap mount.
- Off-the-shelf servers often advertise extra tools, which the harness rejects
  by design. Do not weaken the manifest merely to accept a convenient server.
- Shell execution is the most dangerous capability. Keep the allowlist and
  argv-only rule identical to `Policy`.

---

## P0.2: Live local-model and TUI smoke test

### Goal

Prove the TUI works with the real local model, real streaming, real approvals,
and real MCP tools. The deterministic demo proves orchestration wiring but uses a
scripted model.

### Detailed steps

1. **Confirm the model server.**
   - Check that `llama-server` is listening on `http://127.0.0.1:8080`.
   - Query the model list:

     ```sh
     curl -s http://127.0.0.1:8080/v1/models | jq -r '.data[].id'
     ```

   - Choose a model that supports the chat template and tool-call behavior you
     intend to use. The ignored smoke test only checks text transport, not tool
     calls.

2. **Run the transport smoke test.**

   ```sh
   LLM_BASE=http://127.0.0.1:8080 \
   LLM_MODEL="<model-id>" \
   cargo test --test llm_openai_compat_tests real_llama_server_smoke_test \
     -- --ignored --exact --nocapture
   ```

   Acceptance: non-empty streamed text, no transport error, no secret leakage.

3. **Start Jev or document the fallback.**
   - Jev is optional and advisory. If it is not running at
     `http://127.0.0.1:9090`, confirm the task still routes conservatively and
     does not claim completion without verification.
   - If Jev is required for the pilot, start it and verify loopback-only access.

4. **Run a mock-server TUI smoke test first (optional but useful).**
   - Build the mock test server:

     ```sh
     cargo build --bin mock_mcp_server
     ```

   - Configure `read_only` with the absolute path to
     `target/debug/mock_mcp_server` and Bubblewrap.
   - Use `--tools read_file` for the filesystem slot and
     `--tools status,diff` for the Git slot.
   - This validates the TUI/streaming/event path before the production MCP
     servers are ready.
   - Do not present this as production MCP validation.

5. **Run the real TUI read-only pilot.**
   - Use a disposable repository, not the harness repository.
   - Configure real filesystem and Git MCP servers from P0.1.
   - Start:

     ```sh
     HARNESS_MCP_PROFILE=read_only \
     HARNESS_ROOT=/path/to/disposable/repo \
     cargo run
     ```

   - Submit a small read-only task, for example: inspect `Cargo.toml`, list test
     entry points, and report without modifying files.
   - Observe streaming text, tool events, final completion, and audit output.

6. **Run a mutation pilot with approval.**
   - Switch to `workspace_mutation` and include the shell server.
   - Submit a small change with a test.
   - Confirm the approval prompt shows only bounded metadata, approve it, and
     confirm verification runs before completion.
   - Deny a second mutation and confirm the action does not execute.

7. **Exercise cancellation and recovery.**
   - Start a task, press `Ctrl-C`, and confirm cancellation is visible and never
     becomes completion.
   - Quit while idle and confirm the terminal is restored.
   - Check for orphaned MCP processes with `pgrep -af`.
   - Restart and confirm audit evidence is append-only.

### Acceptance criteria

- TUI starts only after configuration and MCP startup succeed.
- Streaming text appears; hidden reasoning and raw tool arguments do not.
- A read-only task completes with real tools and no mutation.
- A mutation requires approval and cannot complete without verification.
- Denial, timeout, cancellation, and quit behave as documented.
- Audit contains task, tool, approval, verification, and Wiki records.
- No orphaned MCP processes remain after normal shutdown.

---

## P0.3: Real-repository acceptance pilot

### Goal

Demonstrate the harness on a real, disposable copy of a repository with the
production MCP servers, and record the result as operator evidence.

### Detailed steps

1. **Prepare an isolated workspace.**
   - Copy or clone the target repository to a disposable path.
   - Ensure no uncommitted work is at risk.
   - Confirm the repository's verification commands are supported by
     `src/agent/verify.rs` (Cargo, npm scripts, pytest).
   - If unsupported, stop and handle P2.3 before the pilot.

2. **Record baseline evidence.**
   - Save `git status`, `git diff`, the current commit, and a file hash manifest
     if needed.
   - Note the model, MCP binaries, digests, Bubblewrap version, and harness
     commit/tag.

3. **Run a read-only task.**
   - Ask for repository orientation: entry points, test commands, and one small
     risk.
   - Do not approve any mutation.
   - Confirm no files changed.

4. **Run one mutation task.**
   - Choose a small, reversible change with a test.
   - Approve exactly one write or patch.
   - Let harness verification run.
   - Inspect `git diff` and the audit.

5. **Run a failed-verification recovery task.**
   - Introduce or select a failing test, let verification fail, then approve a
     fix.
   - Confirm the task remains blocked until verification passes.
   - Confirm the audit records both the failure and the recovery.

6. **Run recovery drills from `docs/OPERATIONS.md`.**
   - Invalid audit path: startup fails before the TUI.
   - Missing MCP executable: startup fails closed.
   - Digest mismatch: startup fails closed.
   - Model unavailable: task fails explicitly, no false completion.
   - Audit parent unwritable: task fails rather than silently continuing.
   - Terminal recovery: `reset` restores a broken terminal.

7. **Archive evidence.**
   - Preserve `.harness-audit/audit.jsonl`, task summaries, `git diff`, and the
     deployment notes.
   - Do not commit audit evidence or secrets to the repository.
   - Record the exact commands and outcomes in the operator notes.

### Acceptance criteria

- Read-only task makes no changes.
- Mutation task changes only the intended file and passes verification.
- Failed verification blocks completion until a passing fix.
- Recovery drills fail closed with actionable errors.
- Evidence is bounded, redacted, and sufficient to reconstruct what happened.
- No secrets, raw source payloads, or raw prompts appear in tool audit records.

---

## P1.1: Release build, install layout, and checksums

### Goal

Produce a repeatable, installable artifact instead of relying on a debug build in
`target/debug/`.

### Detailed steps

1. **Build locked release artifacts.**

   ```sh
   cargo build --release --locked
   cargo build --release --locked --bins
   ```

   Expected harness binary: `target/release/local-agent-harness`.
   Expected MCP binaries: `target/release/filesystem_mcp`,
   `target/release/git_mcp`, `target/release/shell_mcp` once P0.1 exists.

2. **Decide the install layout.**
   - Example:

     ```text
     /opt/harness/bin/local-agent-harness
     /opt/harness/bin/filesystem_mcp
     /opt/harness/bin/git_mcp
     /opt/harness/bin/shell_mcp
     /opt/harness/share/doc/README.md
     /opt/harness/share/doc/SECURITY.md
     /opt/harness/share/doc/OPERATIONS.md
     /opt/harness/etc/harness.env.example
     ```

   - Keep operator configuration outside the repository.
   - Do not install the mock MCP server for production use.

3. **Add a launcher/config template.**
   - Provide an example env file with fixed MCP paths, Bubblewrap mode, digest
     allowlist, audit path, and local model endpoint.
   - Make the launcher fail closed if required variables are missing.
   - Do not embed credentials.

4. **Add checksums and provenance.**
   - Generate `SHA256SUMS` for the harness and MCP binaries.
   - Record the source commit, build command, Rust version, and target triple.
   - Optionally sign the checksums if the deployment requires it.

5. **Verify the installed artifact.**
   - Run the installed binary with `--headless` against a disposable repository.
   - Run the TUI once.
   - Confirm the installed MCP binaries pass digest pinning.

### Acceptance criteria

- A clean machine can install from the artifact and run `--headless`.
- The release binary path is documented and reproducible.
- Checksums match the installed binaries.
- No debug-only path or test mock is required for production.
- The install does not depend on the source checkout remaining present.

---

## P1.2: CI and release automation

### Goal

Make the accepted baseline continuously verifiable and make release artifacts
reproducible.

### Detailed steps

1. **Confirm the existing CI lanes.**
   - Check the first GitHub Actions run for `stability-demo-review`.
   - Confirm the `test` lane runs default and remote tests, fmt, check, and
     strict Clippy.
   - Confirm the `sandbox` lane installs Bubblewrap, runs positive containment
     tests, runs the production-safe demo, and uploads `demo-evidence`.
   - If the demo artifact is missing, fix the path or permissions.

2. **Add a release workflow on tags.**
   - Trigger on version tags such as `v*`.
   - Run `cargo build --release --locked --bins`.
   - Run the full test suite and demo in the release lane.
   - Package binaries, docs, config template, and `SHA256SUMS`.
   - Upload the package as a workflow artifact.
   - Do not publish automatically unless explicitly approved.

3. **Keep live-model tests out of CI.**
   - The live llama-server smoke test is intentionally ignored.
   - Run it manually during model upgrades or pilot setup.
   - Record the model, llama.cpp version, and result in deployment notes.

4. **Add a lightweight documentation check.**
   - Verify links in README/OPERATIONS/demo docs.
   - Verify the config template names match `Config::from_env`.
   - Optionally run a Markdown linter if the repository adopts one.

### Acceptance criteria

- The default CI lane is green.
- The required Bubblewrap lane is green and uploads demo evidence.
- A tag produces a checksummed release artifact.
- CI never requires a live model or network endpoint.

---

## P2.1: TUI and CLI first-run usability

### Goal

Reduce operator setup mistakes before the first task.

### Detailed steps

1. Add `--help` and `--version` handling in `src/main.rs`.
2. Reject unknown arguments instead of silently ignoring them.
3. Show a startup summary in the TUI or preflight output:
   - repository root
   - MCP profile and started server count
   - audit path
   - model and endpoint profile
   - Jev availability warning
4. Add a clear warning when `HARNESS_MCP_PROFILE=none` means no tools are
   available.
5. Consider a `--doctor` mode that validates config, MCP startup, audit path, and
   model reachability without opening the TUI.
6. Add tests for argument parsing and startup summaries.

### Acceptance criteria

- `--help` and `--version` work without starting MCP servers.
- Unknown arguments fail clearly.
- An operator can see which profile, tools, audit path, and model are active.
- No new secrets or raw paths are printed in logs.

---

## P2.2: Audit retention, rotation, and permissions

### Goal

Keep local evidence bounded over many tasks and readable only by the operator.

### Detailed steps

1. Set restrictive permissions on the audit directory (`0700`) and file
   (`0600`) on Unix.
2. Add an optional maximum total audit size or a rotation policy.
3. Define rotation behavior:
   - rotate only between tasks, never during a task
   - preserve append-only semantics
   - never rewrite existing evidence
4. Document backup and retention in `docs/OPERATIONS.md`.
5. Add tests for permissions and rotation boundaries.
6. Keep the existing 16 KiB per-record bound.

### Acceptance criteria

- Audit files are not world-readable by default.
- Rotation never splits a task's evidence ambiguously.
- The default path remains denied to model file tools.
- Operators have a documented archive/remove procedure.

---

## P2.3: Verifier coverage and configuration

### Goal

Make verification predictable for the repositories actually used, without
letting the model choose verification commands.

### Detailed steps

1. Record which ecosystems the pilot repository uses.
2. Confirm the existing detection:
   - Rust: `cargo fmt --check`, `cargo check --all-targets`, `cargo test`
   - Node: `npm run test`, `npm run build`, `npm run typecheck` when present
   - Python: `pytest -q`
3. If the target repository needs another verifier, add a fixed,
   operator-configured verification profile rather than a model-controlled
   command.
4. Keep verification commands outside the MCP sandbox unless a deliberate
   sandboxed-verifier design is accepted.
5. Add tests for unsupported or ambiguous repositories; they must block, not
   silently complete.
6. Document the supported verification matrix in `README.md` and
   `docs/OPERATIONS.md`.

### Acceptance criteria

- A repository with no supported verifier becomes `Blocked`.
- Verification commands are deterministic and harness-selected.
- The model cannot widen or replace verification commands.
- The pilot repository's verification path is documented and reproducible.

---

## P3: Explicitly deferred work

Do not start these without a new product decision and a security review:

- Dynamic MCP discovery or model-selected servers.
- Arbitrary MCP tool dispatch beyond the fixed capability manifest.
- Git commit, push, reset, checkout, clean, or submodule mutation.
- Package installation or dependency updates.
- Generic network access from tools or verification.
- Shell command strings instead of argv.
- Separate work-API adapters unless the deployed contract diverges.
- Multi-tenant or concurrent task execution.

---

## Recommended execution order

1. Implement and test the production MCP servers (P0.1).
2. Build and pin their release binaries; validate `--headless`.
3. Run the live model transport smoke test (P0.2).
4. Run the TUI read-only pilot, then the approved mutation pilot.
5. Run the real-repository acceptance and recovery drills (P0.3).
6. Package the release and add checksums (P1.1).
7. Confirm CI and add the release workflow (P1.2).
8. Add TUI usability, audit permissions, and verifier coverage as follow-up
   hardening (P2).

## Definition of done for the local deployment

- [ ] Real filesystem, Git, and shell MCP servers exist and are reviewed.
- [ ] Their tool names and schemas exactly match the fixed manifest.
- [ ] All three are pinned by canonical path and SHA-256 digest.
- [ ] `--headless` passes with `read_only` and `workspace_mutation`.
- [ ] The live local model passes the transport smoke test.
- [ ] A read-only TUI task completes with real tools and no mutation.
- [ ] An approved mutation passes harness verification and updates the Wiki.
- [ ] Failed verification blocks completion until fixed.
- [ ] Approval denial, timeout, cancellation, and quit are verified.
- [ ] Audit evidence is bounded, redacted, append-only, and operator-readable.
- [ ] Audit permissions are restrictive by default.
- [ ] Recovery drills in `docs/OPERATIONS.md` have been exercised.
- [ ] A release artifact with checksums installs on a clean machine.
- [ ] Default and required-sandbox CI lanes are green.
- [ ] The operator can reproduce setup from the documentation without reading
      source code.

## Verification commands for the current baseline

```sh
cargo fmt --check
cargo check --all-targets
cargo check --all-targets --features remote
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features remote -- -D warnings
cargo test
cargo test --features remote
./demo/run.sh
```

Expected baseline at `stability-demo-review`:

```text
default: 252 passed, 0 failed, 2 ignored
remote:  251 passed, 0 failed, 2 ignored
```

Do not move accepted tags. Commit and tag new work only after explicit approval.
