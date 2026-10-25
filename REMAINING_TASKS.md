# Remaining Tasks

This is the active working backlog for turning the accepted harness foundation
into a usable, repeatable **local skills-and-scripts deployment**.

It is intentionally more detailed than `HARNESS_PLAN.md`.

> **Architecture decision:** Fixed reviewed skills and fixed helper scripts are
> the active execution substrate. MCP remains preserved as accepted
> prototype/reference work, but is not required for the P0 local workflow.

---

## Baseline and Status

### Accepted historical baseline

The following accepted tags remain immutable:

| Tag | Meaning |
| --- | --- |
| `pre-endpoint-sandbox` | Reconstructed editor-snapshot baseline; not original historical Git state |
| `m12-endpoint-sandbox-review` | Endpoint validation and fail-closed sandbox review |
| `operational-hardening-review` | MCP provenance, digest allowlists, and required containment CI review |
| `stability-demo-review` | Deterministic demonstration, runbook, and stability review |

At `stability-demo-review`, the reported baseline was:

- Commit: `3b5f01d Add production-safe stability demo and audit evidence`
- Working tree: clean before later planning documents
- Tests: 252 default / 251 remote passing, with two intentionally ignored tests
- Demo: passing under Bubblewrap with approximately 68 KiB of bounded evidence
- Debug binary: `target/debug/local-agent-harness`
- No release package or installation layout
- No production MCP server distribution; demonstration coverage used
  `tests/support/mock_mcp_server.rs`

### Subsequent MCP prototype work

The following MCP-related work exists but is **not the active P0 direction**:

- Read-only `filesystem_mcp` implementation in:
  - `src/mcp/fs_server.rs`
  - `src/bin/filesystem_mcp.rs`
  - `tests/filesystem_mcp_tests.rs`
- Parent-directory symlink-escape coverage
- 1 MiB inbound JSON-RPC request-line bound
- Reported test count after that work:
  - 266 default passing
  - 265 remote passing
  - 2 intentionally ignored

This work should be retained, tested, and documented as a future adapter or
reference implementation. Do not expand it into production Git/shell MCP
servers during the active P0 sequence.

---

# What Exists

| Area | State | Notes |
| --- | --- | --- |
| Single-task orchestrator | Implemented | `src/orchestrator.rs`, `src/state_machine.rs`; explicit Learn/Plan/Act/Verify/Review/Compile lifecycle |
| TUI | Implemented | `src/tui/`; Ratatui/crossterm, task entry, streaming, approvals, cancellation, help, detail pane |
| Headless preflight | Implemented | `src/main.rs` `--headless`; validates configuration and startup behavior |
| Local LLM client | Implemented | `src/llm/openai_compat.rs`; bounded OpenAI-compatible streaming, redaction, endpoint validation |
| Jev/System 1 client | Implemented, optional | `src/jev/`; advisory-only routing and scoring |
| Approval gate | Implemented | `src/approval.rs`; mutation approval, timeout, denial, stale-ID handling |
| Policy foundation | Implemented | `src/policy.rs`; root-relative paths, sensitive paths, symlink checks, program allowlist, no shell strings |
| Verification engine | Implemented | `src/agent/verify.rs`; deterministic Cargo, Node, and Python profiles |
| Audit | Implemented | `src/audit.rs`; append-only bounded JSONL evidence |
| Wiki memory | Implemented | `src/wiki/`; compiled `.ai-wiki/`, redaction, sessions, atomic persistence |
| Local/approved-remote profiles | Implemented | Local loopback default; explicit approved HTTPS origins; no fallback |
| Optional hardened sandbox | Implemented | Bubblewrap fail-closed behavior when configured |
| MCP runtime | Frozen prototype/reference | Bounded JSON-RPC runtime, manifests, process containment, provenance checks |
| Read-only filesystem MCP | Frozen prototype/reference | Useful test/reference code; not an active deployment dependency |
| Production-safe demo | Implemented | `tests/demo_tests.rs`, `demo/run.sh`, `demo/README.md` |
| Operator runbook | Implemented | `docs/OPERATIONS.md`; setup, audit, recovery, rollback, limits |
| CI | Partially verified | Default, remote, and Bubblewrap lanes exist; observe/confirm remote CI results separately |

---

# Active Priorities

Priority meanings:

- **P0:** Required before calling the local skills-and-scripts deployment usable.
- **P1:** Required for repeatable installation, handoff, and operational use.
- **P2:** Hardening and usability after the first real pilot.
- **P3:** Deferred. Do not begin without a new product/security decision.

| ID | Outcome | Priority | Depends on |
| --- | --- | --- | --- |
| P0.1 | Fixed Rust script-runner foundation | P0 | Existing policy, audit, orchestration foundation |
| P0.2 | Shared project-root and sensitive-path guard for scripts | P0 | P0.1 |
| P0.3 | Read-only helper scripts and Investigate skill | P0 | P0.1, P0.2 |
| P0.4 | Plan skill and durable plan artifacts | P0 | P0.3 |
| P0.5 | Controlled `write_file` capability | P0 | P0.1, P0.2, existing approval gate |
| P0.6 | Script-backed deterministic verifier | P0 | P0.1, existing verification engine |
| P0.7 | Change-mode end-to-end pilot | P0 | P0.3–P0.6 |
| P0.8 | Live local-model and TUI pilot | P0 | P0.7 |
| P0.9 | Disposable real-repository acceptance and recovery pilot | P0 | P0.8 |
| P1.1 | Local checkpoint-commit workflow | P1 | P0.9 |
| P1.2 | Release build, install layout, checksums, launcher/config template | P1 | P0.7 |
| P1.3 | CI/release automation and observed green CI runs | P1 | P1.2 |
| P2.1 | TUI/CLI first-run usability and doctor mode | P2 | P0.7 |
| P2.2 | Audit retention, rotation, and restrictive permissions | P2 | Existing audit |
| P2.3 | Additional fixed verifier profiles | P2 | P0.9 findings |
| P2.4 | First-class microtask manifests/runtime | P2 | P1.1, pilot findings |
| P3 | MCP expansion, networked tools, package installation, Git mutation, work API specialization | P3 | New decision |

---

# P0.1: Fixed Rust Script Runner

## Goal

Implement a Rust-owned script runner that maps a fixed internal action identifier
to a canonical reviewed helper script.

The model requests a known action. It never chooses:

- a script path;
- an executable;
- an interpreter;
- a working directory;
- a shell command;
- inline source code.

## Proposed modules

```text
src/script_runner/
├── mod.rs
├── actions.rs
├── protocol.rs
└── runner.rs
```plaintext

Suggested action mapping:

```text
ReadFile      → scripts/read_file.py
GitStatus     → scripts/git_status.py
GitDiff       → scripts/git_diff.py
WriteFile     → scripts/write_file.py
RunVerifier   → scripts/run_verifier.py
```plaintext

## Required behavior

1. Define strict internal action types.
2. Map actions to canonical harness-owned script paths.
3. Start scripts with argv only, for example:

   ```text
   python3 /canonical/harness/scripts/read_file.py
   ```plaintext

4. Prohibit `sh -c`, `bash -c`, `python -c`, model-provided executable paths,
   and model-provided script paths.
5. Send one bounded JSON request through stdin.
6. Parse one bounded JSON response from stdout.
7. Capture bounded stderr only for diagnostics/audit.
8. Enforce hard timeout and cancellation.
9. Kill child processes on timeout, cancellation, or runner drop.
10. Use a minimal child environment.
11. Record script identity, action ID, duration, and bounded result metadata in
    audit evidence.

## Initial protocol

Request:

```json
{
  "action": "read_file",
  "project_root": "/canonical/project/root",
  "path": "src/lib.rs",
  "line_start": 1,
  "line_end": 80
}
```plaintext

Success response:

```json
{
  "ok": true,
  "data": {
    "path": "src/lib.rs",
    "content": "bounded UTF-8 text",
    "truncated": false
  }
}
```plaintext

Failure response:

```json
{
  "ok": false,
  "error": {
    "code": "PATH_OUTSIDE_PROJECT_ROOT",
    "message": "The requested path is not permitted."
  }
}
```plaintext

## Acceptance criteria

- The runner accepts only known fixed action IDs.
- The action-to-script mapping is not model-controlled.
- Mock scripts cover successful execution, malformed JSON, nonzero exit,
  timeout, cancellation, oversized stdout, oversized stderr, and missing script.
- Protocol errors become bounded structured action outcomes.
- No shell process is used.
- The child environment does not inherit model credentials, proxy variables, or
  cloud credentials.
- `cargo fmt --check`, checks, Clippy, and tests remain clean.

---

# P0.2: Shared Project-Root and Sensitive-Path Guard

## Goal

Create one shared path-policy implementation used by both the Rust orchestrator
and helper scripts.

The existing policy code is a strong foundation, but the script substrate must
not rely on an orchestrator-only lexical check.

## Required behavior

1. Canonicalize configured project root at startup.
2. Reject absolute paths.
3. Reject NUL bytes.
4. Reject lexical traversal outside root.
5. Canonicalize existing file paths and reject escapes.
6. Canonicalize the nearest existing parent for new-file paths.
7. Reject symlinks resolving outside root.
8. Deny harness-owned paths, including audit and Wiki storage where configured.
9. Deny sensitive files and directories.
10. Return stable structured error codes.

Initial denied patterns include:

```text
.env
.env.*
*.pem
*.key
id_rsa
id_ed25519
credentials*
secrets*
.aws/
.gcp/
.azure/
.ssh/
.kube/
```plaintext

## Acceptance criteria

- The same path policy is applied before dispatch and inside filesystem scripts.
- Tests cover absolute paths, `..`, NUL bytes, existing-file symlink escapes,
  parent-directory symlink escapes, sensitive paths, and valid nested paths.
- New-file validation cannot escape through a symlinked parent.
- Errors are bounded and safe for model/UI display.
- Policy denial does not create an approval prompt.

---

# P0.3: Read-Only Helper Scripts and Investigate Skill

## Goal

Deliver the first useful local workflow without mutation:

```text
Investigate task
→ inspect bounded repository evidence
→ compile evidence brief
→ complete without changing repository files
```plaintext

## Helper scripts

### `scripts/read_file.py`

Requirements:

- accepts relative path and optional line range;
- uses the shared project-root policy;
- reads bounded UTF-8 text only;
- reports truncation;
- never reads denied sensitive paths;
- documents line indexing; use 1-based inclusive ranges.

### `scripts/git_status.py`

Requirements:

- runs a fixed argv-only Git status command;
- uses configured project root;
- disables pager behavior;
- sets `GIT_OPTIONAL_LOCKS=0`;
- returns bounded machine-readable and display-safe output;
- clearly reports non-Git repositories.

### `scripts/git_diff.py`

Requirements:

- supports fixed `staged: bool`;
- runs only `git diff` or `git diff --cached`;
- uses no shell;
- disables pager behavior;
- returns bounded output and truncation metadata.

## Investigate skill

Implement an operator-selectable `Investigate` mode that exposes only:

```text
read_file
git_status
git_diff
```plaintext

Expected durable artifact:

```text
.ai-wiki/evidence/<task-id>.md
```plaintext

The artifact must distinguish:

- confirmed facts;
- hypotheses;
- missing evidence;
- inspected files;
- Git state;
- recommended next action.

## Acceptance criteria

- A disposable repository can be investigated with no mutation.
- A model sees only the read-only action catalog.
- Read-only action outcomes are bounded before entering model context.
- Evidence brief persists through Wiki/audit compilation.
- Sensitive files, path escapes, and symlink escapes are denied.
- Git output is bounded and does not invoke a pager.
- A cancelled Investigate task does not falsely become complete.

---

# P0.4: Plan Skill and Plan Artifacts

## Goal

Add a read-only Plan workflow:

```text
Learn → Plan → Review → Compile
```plaintext

The task produces a durable implementation plan but does not mutate files.

## Required behavior

1. Reuse bounded evidence from Investigate/read-only actions.
2. Request a concise model plan with:
   - goal;
   - likely files;
   - ordered steps;
   - risks;
   - assumptions;
   - expected verification profile.
3. Preserve the rule that verification selection remains harness-owned.
4. Save plan artifact:

   ```text
   .ai-wiki/plans/<task-id>.md
   ```plaintext

5. Allow operator review before starting a separate Change task.

## Acceptance criteria

- Plan mode exposes no mutation actions.
- A plan artifact is bounded, redacted, and durable.
- Unsupported/missing verifier information is explicitly marked as a blocker or
  operator decision—not silently invented.
- The final plan distinguishes repository evidence from model inference.

---

# P0.5: Controlled Write Capability

## Goal

Add the narrowest safe write capability for Change mode.

This work supersedes the old MCP `write_file` / `apply_patch` implementation
priority. `apply_patch` is not part of the initial script-substrate release.

## Required delivery sequence

### P0.5a — Schema and rejection behavior

Implement typed `WriteFile` action parsing and policy preflight.

Required fields:

```text
path: relative path
content: UTF-8 text
expected_sha256: optional lowercase SHA-256
```plaintext

### P0.5b — New file creation only

Allow creation only when the target does not exist.

Requirements:

- explicit approval;
- bounded file size;
- parent remains inside project root;
- temporary sibling file plus atomic rename;
- cleanup after failure;
- audit evidence records create action.

### P0.5c — Guarded overwrite

Allow replacement only when:

```text
expected_sha256 == SHA-256(current file content)
```plaintext

The check must occur immediately before replacement.

Do not add patch support until a separately reviewed design decision.

## Approval requirements

Approval follows:

```text
parse
→ policy preflight
→ static skill/action preflight
→ bounded approval prompt
→ matching active approval
→ script dispatch
```plaintext

Denial, expiration, cancellation, stale IDs, mismatches, and absent approval
must never execute a write.

## Acceptance criteria

- New-file creation requires approval.
- Existing-file overwrite requires a matching expected SHA-256.
- SHA mismatch leaves the original target unchanged.
- Failed writes leave no partial target or temporary artifact.
- Policy-denied paths do not trigger approval.
- Audit records the action, approval, target metadata, and result without
  storing full file content.
- Verification is marked stale after a successful write.

---

# P0.6: Script-Backed Deterministic Verification

## Goal

Run existing harness-selected verifier profiles through a fixed action/script
boundary without allowing arbitrary commands.

## Requirements

1. Preserve existing verifier detection and profiles as the source of truth.
2. Represent each verification step as:

   ```rust
   pub struct VerificationCommand {
       pub program: String,
       pub args: Vec<String>,
       pub cwd: std::path::PathBuf,
       pub timeout: std::time::Duration,
   }
   ```plaintext

3. `scripts/run_verifier.py` receives only harness-selected commands.
4. Never use `sh -c`.
5. Do not install dependencies automatically in the first release.
6. If dependencies or a supported profile are missing, block with actionable
   remediation.
7. Record exit code, timeout status, duration, and bounded output tail.
8. Require successful verification after every mutation.

## Initial profiles

| Repository signal | Fixed verification |
| --- | --- |
| `Cargo.toml` | `cargo fmt --check`; `cargo check --all-targets`; `cargo test` |
| Supported Python profile | fixed configured `pytest -q` |
| Supported Node profile | fixed configured test/build/type-check scripts |
| Unsupported/ambiguous project | Block |

## Acceptance criteria

- The model cannot specify a verification command.
- A mutation cannot complete without passing required checks.
- Failing verification returns bounded corrective evidence and returns to Act or
  becomes Blocked within configured retry limits.
- Missing verifier/dependencies block completion.
- No generic shell authority is added.

---

# P0.7: Change-Mode End-to-End Pilot

## Goal

Prove one complete bounded local change using fixed scripts:

```text
Learn
→ Plan
→ approve one write
→ verify
→ review
→ compile evidence
→ complete
```plaintext

## Pilot requirements

1. Use a disposable repository.
2. Use a local OpenAI-compatible model endpoint.
3. Start with one narrow change:
   - add a small text/config/source file; or
   - update a small existing text file with expected SHA-256.
4. Approve exactly one mutation.
5. Run harness-selected verification.
6. Inspect Git status and diff through read-only scripts.
7. Persist audit and Wiki artifacts.
8. Confirm failure behavior with one deliberately failing verification run.

## Acceptance criteria

- Only intended files change.
- Approval metadata is bounded and redacted.
- Verification passes before completion.
- Failed verification prevents completion.
- Final report distinguishes verified facts from known limits.
- No MCP server is needed for the workflow.

---

# P0.8: Live Local-Model and TUI Pilot

## Goal

Validate real local model streaming, TUI events, approval flow, cancellation, and
script-backed actions.

## Steps

1. Start a local OpenAI-compatible endpoint, typically `llama-server`, on the
   configured loopback address.
2. Run the ignored transport smoke test where available.
3. Start a read-only Investigate task against a disposable repository.
4. Confirm visible streamed text appears without raw reasoning persistence.
5. Run a Change task using one approved write and deterministic verification.
6. Deny a second requested mutation.
7. Exercise `Esc`, `Ctrl-C`, normal quit, and terminal restoration.
8. Confirm no orphaned helper-script processes remain.

## Acceptance criteria

- TUI starts only after configuration validation succeeds.
- Read-only task completes without mutation.
- Mutation requires approval and passing verification.
- Denial, timeout, cancellation, and quit are visible and fail safely.
- Audit/Wiki records are append-only and bounded.
- No unintended remote endpoint or fallback is used.

---

# P0.9: Disposable Real-Repository Acceptance Pilot

## Goal

Demonstrate the full workflow on a disposable copy of a real repository and
exercise recovery procedures.

## Required scenarios

1. **Read-only orientation**
   - identify entry points, test commands, and one bounded risk;
   - confirm no files change.

2. **Verified bounded mutation**
   - apply one approved small change;
   - verify;
   - inspect status/diff;
   - save evidence.

3. **Failed-verification recovery**
   - run a change that fails a verifier;
   - confirm task cannot complete;
   - apply an approved remediation;
   - confirm passing verification is required before completion.

4. **Operational failure drills**
   - invalid audit path;
   - missing helper script;
   - script digest/provenance mismatch if enabled;
   - model unavailable;
   - unwritable audit parent;
   - terminal recovery procedure.

## Acceptance criteria

- Read-only tasks change nothing.
- Verified changes affect only approved scope.
- Failed verification blocks completion.
- Operational failures fail closed with actionable errors.
- Evidence is bounded, redacted, and sufficient to reconstruct the task.
- Audit entries do not contain secrets, raw prompts, or full source payloads.

---

# P1.1: Local Git Checkpoint Workflow

## Goal

Add a narrow operator-approved local checkpoint workflow after verified
microtasks.

Git is a recovery and review mechanism, not the security boundary.

## Scope

Initially, implement the workflow and operator confirmation model before
exposing any Git mutation to the model.

Required pre-checkpoint report:

- modified tracked files;
- added tracked files;
- deleted tracked files;
- untracked files;
- diff summary;
- verifier result;
- task/microtask identity.

A checkpoint may be created only after:

1. required verification passes;
2. the operator has reviewed the diff;
3. unexpected untracked files are addressed;
4. the operator explicitly approves checkpoint creation.

## Not in scope

```text
push
fetch
pull
reset
checkout
clean
rebase
merge
tag
submodules
history rewrite
```plaintext

## Acceptance criteria

- Checkpoint documentation clearly distinguishes tracked rollback from untracked
  file recovery limitations.
- Commit metadata records task ID, microtask ID, verifier status, and changed
  file count.
- No automatic commit policy is enabled by default.
- Recovery drills cover reverting a checkpoint in a disposable repository.

---

# P1.2: Release Build and Installation Layout

## Goal

Produce a repeatable installation that does not depend on a source checkout or
debug binary.

## Requirements

1. Build release artifacts:

   ```sh
   cargo build --release --locked
   ```plaintext

2. Package:

   ```text
   local-agent-harness
   reviewed helper scripts
   README.md
   SECURITY.md
   OPERATIONS.md
   example configuration
   SHA256SUMS
   ```plaintext

3. Document a fixed installation layout.
4. Provide launcher/config template for:
   - project root;
   - audit location;
   - local model endpoint;
   - optional approved-remote endpoint profile;
   - script location;
   - optional hardened sandbox profile.
5. Record source commit, build command, Rust version, target triple, and
   artifact checksums.

## Acceptance criteria

- A clean machine can install and run `--headless`.
- The runtime finds helper scripts from the installed location.
- Checksums match installed binaries and scripts.
- No test mock, debug path, or source checkout is required.

---

# P1.3: CI and Release Automation

## Goal

Continuously verify the active script-substrate path and produce reproducible
release artifacts.

## Requirements

1. Preserve existing default, remote-feature, and hardened-sandbox CI coverage.
2. Add script-runner protocol and helper-script tests to CI.
3. Add release workflow for version tags:
   - release build;
   - full tests;
   - package creation;
   - `SHA256SUMS`;
   - artifact upload.
4. Keep live local-model tests out of normal CI.
5. Verify documentation/config template names match runtime configuration.

## Acceptance criteria

- Default CI is green.
- Remote feature CI is green.
- Required Bubblewrap CI remains green where the hardened profile is enabled.
- Release tags generate checksummed artifacts.
- CI does not require a live model, cloud endpoint, or external network access.

---

# P2.1: TUI and CLI First-Run Usability

## Goal

Reduce configuration and operator mistakes before a task begins.

## Tasks

1. Add `--help`.
2. Add `--version`.
3. Reject unknown arguments.
4. Add startup summary:
   - project root;
   - selected task mode;
   - available skills;
   - audit path;
   - endpoint profile;
   - Jev availability;
   - hardened-profile status.
5. Add `--doctor`:
   - validate config;
   - validate script paths;
   - validate audit location;
   - validate root;
   - optionally validate model reachability;
   - report without opening the TUI.
6. Avoid exposing secrets, token values, or unsafe raw paths.

## Acceptance criteria

- `--help` and `--version` do not start worker processes.
- `--doctor` clearly reports blocking setup errors.
- Operators can identify active mode, skills, audit path, and endpoint profile.

---

# P2.2: Audit Retention, Rotation, and Permissions

## Goal

Keep local evidence bounded and operator-readable over many tasks.

## Tasks

1. Default audit directory permissions to `0700` on Unix.
2. Default audit file permissions to `0600` on Unix.
3. Add optional retention/maximum-size policy.
4. Rotate only between tasks.
5. Preserve append-only semantics.
6. Never split one task’s evidence ambiguously.
7. Document archive and deletion procedure.

## Acceptance criteria

- Audit evidence is not world-readable by default.
- Rotation preserves complete task trails.
- Model file skills cannot read harness audit paths.
- Existing per-record bounds remain enforced.

---

# P2.3: Additional Fixed Verifier Profiles

## Goal

Support pilot repositories without allowing model-selected verification.

## Tasks

1. Record verification requirements found during P0.9 pilots.
2. Add reviewed static profiles only when justified.
3. Keep commands argv-only and bounded.
4. Block unsupported projects rather than guessing.
5. Update documentation and tests for each supported profile.

## Acceptance criteria

- Verification remains harness-selected.
- Unsupported projects become `Blocked`.
- New profiles have deterministic tests and documented prerequisites.
- Package installation remains out of scope unless separately approved.

---

# P2.4: First-Class Microtask Support

## Goal

Move microtask planning from operator convention to a constrained runtime concept
only after pilots validate the workflow.

## Candidate microtask fields

```text
microtask_id
goal
non_goals
allowed_files
mode
allowed_skills
required_verifier_profile
completion_rule
checkpoint_policy
```plaintext

## Constraints

- Microtasks cannot grant additional authority.
- Allowed-file restrictions supplement—not replace—project-root policy.
- Model-generated microtasks require operator review before use.
- No auto-commit by default.

## Acceptance criteria

- Microtask identity appears in audit and Wiki artifacts.
- Microtask allowed-file scope is enforced before mutation approval.
- Checkpoint behavior remains explicit and operator-controlled.

---

# P3: Deferred Work

Do not begin the following without a new product decision and security review:

- dynamic MCP discovery;
- arbitrary MCP dispatch;
- remote MCP servers;
- generic shell execution;
- shell command strings;
- model-selected scripts/executables;
- arbitrary Python execution;
- package installation;
- dependency upgrades;
- generic network access;
- Git push/fetch/reset/checkout/clean/rebase/merge;
- automatic commits;
- patches or generalized multi-file transformations;
- work-API specialization when OpenAI-compatible behavior remains sufficient;
- multi-user or multi-tenant operation;
- concurrent independent tasks;
- autonomous self-modification.

---

# Recommended Execution Order

```text
1. P0.1 — Fixed script runner
2. P0.2 — Shared root/path guard
3. P0.3 — Read-only scripts and Investigate mode
4. P0.4 — Plan mode
5. P0.5 — Controlled write support
6. P0.6 — Script-backed verifier
7. P0.7 — Change-mode E2E pilot
8. P0.8 — Live local-model and TUI pilot
9. P0.9 — Disposable real-repository acceptance pilot
10. P1.1 — Local checkpoint workflow
11. P1.2 — Release package
12. P1.3 — CI/release automation
13. P2 follow-up hardening and usability
```plaintext

---

# Definition of Done: First Usable Local Deployment

- [ ] Fixed Rust script runner exists and dispatches only canonical reviewed scripts.
- [ ] Project-root, symlink, and sensitive-path restrictions are enforced before
      and inside filesystem-capable scripts.
- [ ] Read-only `read_file`, `git_status`, and `git_diff` scripts work in a
      disposable repository.
- [ ] Investigate and Plan modes create durable bounded artifacts without
      mutation.
- [ ] A write requires an explicit matching approval.
- [ ] New-file writes are atomic and bounded.
- [ ] Existing-file replacement requires expected SHA-256.
- [ ] A mutation requires deterministic verification before completion.
- [ ] Failed verification blocks completion.
- [ ] Audit and Wiki evidence are bounded, redacted, append-only, and local.
- [ ] A live local-model/TUI pilot succeeds without MCP runtime dependency.
- [ ] Recovery drills have been performed against a disposable repository.
- [ ] Release artifacts install without a source checkout or debug binary.
- [ ] Default and required hardened-profile CI lanes are green where applicable.

---

# Historical Verification Commands

These commands remain useful for validating the accepted foundation and should be
updated when script-runner work adds new test targets:

```sh
cargo fmt --check
cargo check --all-targets
cargo check --all-targets --features remote
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features remote -- -D warnings
cargo test
cargo test --features remote
./demo/run.sh
```plaintext

Do not move accepted tags. Commit and tag active script-substrate work only after
explicit review and approval.

