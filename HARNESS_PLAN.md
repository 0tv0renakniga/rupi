# Local Agent Harness — Unified Implementation Plan

> **Status:** Active architecture and implementation roadmap  
> **Replaces:** `harness_plan_v1.md` and `harness_plan_v2.md`  
> **Primary direction:** fixed skills and reviewed helper scripts, governed by one
> Rust orchestrator. MCP remains preserved as accepted prototype/reference work,
> but is not the active P0 execution substrate.

---

## 1. Executive Summary

`local-agent-harness` is a local-first Rust harness for bounded repository work.

It supports one agent per task, using a deterministic lifecycle:

```text
Learn → Plan → Act → Verify → Review → Compile → Complete
```plaintext

The model may reason, inspect evidence, propose plans, request available actions,
and draft reports. It does **not** control security boundaries, approval,
verification, task-mode escalation, or completion.

The Rust orchestrator owns:

- task state transitions;
- task budgets;
- policy enforcement;
- project-root confinement;
- mutation approvals;
- fixed action dispatch;
- deterministic verification;
- audit evidence;
- AI Wiki compilation;
- terminal task status.

The active execution architecture is:

```text
Task mode
→ reviewed skill
→ fixed action identifier
→ fixed helper script
→ bounded JSON input/output
→ deterministic verification
→ evidence compilation
```plaintext

The harness is intentionally narrower than a general autonomous-agent platform.
Its first goal is one reliable local workflow:

```text
Investigate a repository
→ create a bounded implementation plan
→ approve one bounded change
→ verify it deterministically
→ review the diff
→ save local evidence
→ optionally create an operator-approved local Git checkpoint
```plaintext

---

# 2. Current Status and Historical Baseline

## 2.1 Accepted implementation work

The following work is accepted and remains valuable as the security and
reliability foundation for the next architecture phase:

- single-task Rust orchestrator and explicit state machine;
- bounded task budgets and cancellation;
- local OpenAI-compatible streaming LLM client;
- bounded SSE parsing and tool-call delta reassembly;
- event-driven Ratatui TUI;
- interactive approval handling;
- deterministic verification engine;
- AI Wiki ingestion, redaction, deterministic compilation, and atomic writes;
- append-only bounded audit evidence;
- Jev/System 1 advisory adapter;
- endpoint validation for local and approved-remote profiles;
- credential-reference handling that does not expose references through logs;
- path, symlink, secret-path, command-metacharacter, timeout, and cancellation
  tests;
- fail-closed Bubblewrap containment when the hardened MCP profile is enabled;
- positive no-network containment evidence;
- executable provenance and optional SHA-256 allowlisting for MCP processes;
- production-safe demonstration and operator recovery runbook.

Accepted review tags include:

| Tag | Meaning |
| --- | --- |
| `pre-endpoint-sandbox` | Reconstructed editor-snapshot baseline; not original historical Git state |
| `m12-endpoint-sandbox-review` | Endpoint profiles and fail-closed sandbox review |
| `operational-hardening-review` | Provenance, digest allowlists, CI containment requirements |
| `stability-demo-review` | Deterministic demonstration, runbook, and stability review |

The reported test baseline at the accepted stability review was:

```text
default build:          252 passed, 1 ignored
--features remote:      251 passed, 1 ignored
```plaintext

The intentionally ignored tests were the live local-model smoke test and the
production-safe demonstration, because they require explicit local runtime
conditions.

## 2.2 Architecture transition

The earlier plans made MCP the primary execution substrate. That work is not
discarded, but it is no longer the active P0 path.

### Frozen MCP-first assumptions

The following are **not** on the immediate implementation path:

- dynamic MCP discovery;
- arbitrary MCP dispatch;
- model-visible server/tool catalogs;
- generic shell MCP actions;
- MCP as a prerequisite for basic filesystem, Git inspection, and verification;
- expansion of the MCP capability manifest.

### Active skills-and-scripts direction

The active path uses:

- a fixed Rust script runner;
- a small fixed helper-script inventory;
- task modes;
- reviewed skills;
- bounded structured action schemas;
- deterministic verifier profiles;
- optional operator-approved checkpoint commits.

MCP may later be reintroduced as an adapter behind the same internal skill and
action interfaces if a real operational need justifies its complexity.

---

# 3. Architectural Principles

## 3.1 One agent per task

One task owns:

- one orchestrator;
- one conversation;
- one selected task mode;
- one task budget;
- one state machine;
- one audit/evidence trail.

Learn, Plan, Act, Verify, Review, and Compile are phases of that one loop—not
independent agents.

## 3.2 Hard gates override model judgment

The model may claim that work is done. That claim is advisory.

A mutation-capable task may complete only after the harness has recorded:

1. approved action evidence;
2. successful mutation outcome;
3. deterministic verification selected by the harness;
4. required verifier exit status `0`;
5. review and memory compilation.

A classifier, Jev verdict, Wiki status, or model statement can never override a
failed or absent verifier.

## 3.3 Fail conservatively

| Situation | Required behavior |
| --- | --- |
| Jev unavailable or low confidence | Use deterministic fallback or System 2 reasoning |
| Model malformed action request | Return bounded structured teaching error |
| Approval unavailable | Deny mutation |
| Approval expires, is stale, or is cancelled | Do not execute action |
| Verifier absent or cannot run | Block task |
| Verification fails | Return to bounded remediation or block |
| Script protocol invalid | Fail action and preserve evidence |
| Endpoint violates profile | Reject configuration; do not fall back |
| Hardened sandbox explicitly configured but unavailable | Fail closed |

## 3.4 Local-first endpoint policy

The default model profile uses loopback endpoints, such as:

```text
http://127.0.0.1:8080/v1/chat/completions
```plaintext

The harness validates endpoint profiles:

- local profile: loopback-only;
- approved remote profile: explicit HTTPS origin allowlist;
- no automatic fallback from local to remote;
- explicit model name required;
- optional bearer token retrieved through a credential reference;
- credential reference never included in `Debug`, errors, audit entries, or UI.

The approved-remote path continues to use the OpenAI-compatible LLM adapter
unless the deployed API contract genuinely differs.

## 3.5 Repository contents are untrusted data

Repository files, comments, documentation, test output, Git history, tool output,
and generated content may contain prompt injection or misleading instructions.

They may inform the model, but they never authorize the model to:

- reveal secrets;
- alter policy;
- expand skills;
- bypass verification;
- change response contracts;
- run commands outside fixed action schemas.

---

# 4. High-Level Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│                              TUI / CLI                              │
│ Task entry · mode selection · display · approve · deny · cancel     │
│ No direct execution, no policy authority, no model-network control  │
└───────────────────────────────┬─────────────────────────────────────┘
                                │ typed user commands
┌───────────────────────────────▼─────────────────────────────────────┐
│                            ORCHESTRATOR                             │
│ State machine · task mode · budgets · policy · approvals · audit    │
│ Wiki context · verification gate · completion · checkpoints         │
└───────────────┬───────────────────────────────────┬─────────────────┘
                │                                   │
                │ bounded model context             │ approved action
┌───────────────▼─────────────────┐   ┌─────────────▼─────────────────┐
│ Local LLM / Optional Jev         │   │ Fixed Rust Script Runner       │
│ OpenAI-compatible local client   │   │ action ID → canonical script   │
│ Jev is advisory only             │   │ argv only · JSON stdin/stdout │
└─────────────────────────────────┘   └─────────────┬─────────────────┘
                                                     │
                         ┌───────────────────────────▼──────────────────────────┐
                         │ Reviewed Helper Scripts                               │
                         │ read_file · git_status · git_diff · write_file        │
                         │ run_verifier · future checkpoint helper               │
                         └──────────────────────────────────────────────────────┘
```text

---

# 5. Task Modes

A task mode is an operator-selected workflow and policy bundle. The model cannot
upgrade its own authority by changing modes.

| Mode | Lifecycle | Mutation | Required output |
| --- | --- | --- | --- |
| `Investigate` | Learn → Review → Compile | Forbidden | Evidence brief |
| `Plan` | Learn → Plan → Review → Compile | Forbidden | Reviewed plan |
| `Change` | Learn → Plan → Act → Verify → Review → Compile | Approval required | Verified change report |
| `Review` | Learn → Review → Compile | Forbidden | Review findings |

## 5.1 Investigate mode

Purpose:

- inspect repository state;
- gather evidence;
- identify likely implementation areas;
- document uncertainty;
- recommend a next step.

Available initial skills:

```text
read_file
git_status
git_diff
compile_evidence_brief
```plaintext

It must not mutate repository content.

## 5.2 Plan mode

Purpose:

- understand a bounded task;
- produce an implementation plan;
- identify likely files and risks;
- define expected verifier profile;
- document assumptions.

A plan is advisory. The harness, not the model, chooses verification commands.

## 5.3 Change mode

Purpose:

- make a bounded approved change;
- verify it deterministically;
- review the resulting diff;
- preserve evidence;
- optionally create an operator-approved local checkpoint commit.

Change mode begins with only narrowly scoped write support. It does not initially
include arbitrary patches, package installation, generic shell execution, or Git
history modification.

## 5.4 Review mode

Purpose:

- inspect existing source or a local diff;
- identify defects, risks, missing tests, and policy concerns;
- compile findings without changing the repository.

---

# 6. Skills and Fixed Action Types

## 6.1 Skill requirements

Every skill must be:

- fixed and version-controlled;
- schema-validated;
- available only in permitted task modes;
- bounded in request size, response size, runtime, and output;
- auditable;
- unable to expand its own authority;
- implemented through a fixed action identifier.

The model may request a skill action only from the small catalog exposed for the
selected mode.

## 6.2 Initial action catalog

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    ReadFile {
        path: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
    },

    GitStatus,

    GitDiff {
        staged: bool,
    },

    WriteFile {
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },

    RunVerifier {
        profile: VerifierProfileId,
    },
}
```plaintext

Initial intentionally absent actions:

```text
apply_patch
run_command
run_shell
install_dependency
git_push
git_fetch
git_reset
git_checkout
git_clean
git_rebase
generic network request
arbitrary MCP tool
```plaintext

## 6.3 Model-facing tool descriptions

Tool descriptions must be terse and mode-specific to reduce local-model context
pressure.

Example Change-mode catalog:

```text
read_file(path, line_start?, line_end?)
git_status()
git_diff(staged)
write_file(path, content, expected_sha256?)
```plaintext

`run_verifier` may be orchestrator-triggered rather than model-visible in the
first version. The model can request verification conceptually, but it cannot
specify the command.

---

# 7. Fixed Helper-Script Substrate

## 7.1 Why scripts replace MCP for P0

A fixed script substrate provides the required execution boundary with less
runtime complexity than MCP:

| Concern | Fixed scripts | Dynamic MCP |
| --- | --- | --- |
| Action inventory | Small and static | Potentially large and dynamic |
| Model context | Minimal descriptions | Tool schemas can consume context |
| Process lifecycle | One-shot bounded child | Long-lived server management |
| Protocol | Bounded JSON stdin/stdout | JSON-RPC lifecycle and discovery |
| Provenance | Canonical script paths | Server binary + protocol + manifest |
| Initial debugging | Direct and simple | Distributed transport behavior |

The script substrate does **not** weaken the core rule:

```text
The model proposes; the orchestrator authorizes and executes.
```plaintext

## 7.2 Script ownership

Scripts are harness-controlled artifacts and must not be sourced from the target
repository.

Recommended layout:

```text
local-agent-harness/
├── src/
├── scripts/
│   ├── read_file.py
│   ├── git_status.py
│   ├── git_diff.py
│   ├── write_file.py
│   └── run_verifier.py
├── docs/
└── tests/
```plaintext

The Rust runner maps an action identifier to a canonical script path.

```text
Action::ReadFile     → scripts/read_file.py
Action::GitStatus    → scripts/git_status.py
Action::GitDiff      → scripts/git_diff.py
Action::WriteFile    → scripts/write_file.py
Action::RunVerifier  → scripts/run_verifier.py
```plaintext

The model cannot provide a script path, executable path, interpreter, working
directory, or inline code.

## 7.3 Invocation requirements

Scripts execute through an argv-only process launch:

```text
python3 /canonical/harness/scripts/read_file.py
```plaintext

Forbidden execution patterns:

```text
sh -c "..."
bash -c "..."
zsh -c "..."
python3 -c "<runtime code>"
```plaintext

The runner must enforce:

- canonical script path;
- fixed interpreter path or reviewed interpreter policy;
- fixed project-root working directory;
- bounded JSON stdin;
- bounded stdout;
- bounded stderr;
- hard timeout;
- cancellation;
- process cleanup;
- structured protocol parsing;
- sanitized child environment.

## 7.4 Script protocol

Each script reads one JSON request from stdin and writes one JSON response to
stdout.

Request example:

```json
{
  "action": "read_file",
  "project_root": "/canonical/project/root",
  "path": "src/lib.rs",
  "line_start": 1,
  "line_end": 80
}
```plaintext

Response example:

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

Error response example:

```json
{
  "ok": false,
  "error": {
    "code": "PATH_OUTSIDE_PROJECT_ROOT",
    "message": "The requested path is not permitted."
  }
}
```plaintext

Only protocol JSON belongs on stdout. Script diagnostics belong on stderr and are
captured only within configured bounds.

---

# 8. Project-Root and Sensitive-Path Policy

## 8.1 Required path checks

All filesystem-capable actions must share one policy implementation.

The policy must reject:

1. absolute paths;
2. empty paths;
3. NUL bytes;
4. lexical traversal outside the project root;
5. canonicalized existing paths outside the root;
6. canonicalized parent directories for new files outside the root;
7. symlink escapes;
8. harness-private paths;
9. secret and credential paths;
10. non-UTF-8 text content when the relevant action requires UTF-8.

## 8.2 Sensitive-path defaults

Deny by default includes patterns such as:

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

The exact deny list should be centrally configured and tested. Read denial and
write denial must use the same policy source.

## 8.3 Action result behavior

Policy errors become bounded structured outcomes. They must not crash the
orchestrator or accidentally create an approval prompt.

Examples:

```text
ACTION_DENIED
SECRET_PATH_DENIED
PATH_OUTSIDE_PROJECT_ROOT
PATH_SYMLINK_ESCAPE
INVALID_UTF8
```plaintext

---

# 9. Approval and Mutation Policy

## 9.1 Approval sequence

Mutating actions follow this strict sequence:

```text
Parse action
→ schema validation
→ mode and skill preflight
→ project-root and secret-path policy checks
→ construct bounded redacted approval request
→ operator decision
→ dispatch script
→ record audit evidence
→ invalidate prior verification
```plaintext

No mutation occurs before approval.

## 9.2 Approval record

An approval request includes:

- unique action ID;
- task ID;
- microtask ID, if applicable;
- action type;
- bounded redacted summary;
- target relative path;
- create vs. replace semantics;
- expected SHA-256 when replacing a file;
- expiration time.

Approval decisions may be:

```text
ApproveOnce
Deny
Expired
Cancelled
Stale
Mismatched
```plaintext

Any state other than an active matching `ApproveOnce` denies dispatch.

## 9.3 Initial write capability

Write support is deliberately incremental:

| Stage | Allowed write behavior |
| --- | --- |
| 1 | Validate schema and reject unsafe requests |
| 2 | Create a new text file only |
| 3 | Replace an existing file only with expected SHA-256 |
| 4 | Consider unified diff / patch support after separate review |

For overwrite behavior:

```text
expected_sha256 == SHA-256(current target content)
```plaintext

must hold immediately before writing.

The script writes to a temporary sibling path and atomically renames it into
place only after a complete successful write.

---

# 10. Deterministic Verification

## 10.1 Verification is harness-owned

The model can explain why verification is needed, but cannot choose arbitrary
commands or mark checks as passed.

A verifier profile is selected by the harness from project evidence and operator
configuration.

```rust
pub struct VerificationCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub timeout: std::time::Duration,
}
```plaintext

No verifier executes through `sh -c`.

## 10.2 Initial profiles

| Project signal | Initial profile |
| --- | --- |
| `Cargo.toml` | `cargo fmt --check`; `cargo check --all-targets`; `cargo test` |
| Python project profile | fixed `pytest -q` or configured project test command |
| Node project profile | configured fixed test/build/type-check scripts |
| Unsupported project | Block until operator adds reviewed profile |

Dependency installation is not part of the initial execution path. If required
dependencies are absent, the harness blocks and reports the prerequisite.

## 10.3 Verification report

```rust
pub struct VerifyReport {
    pub passed: bool,
    pub steps: Vec<VerifyStep>,
    pub bounded_output_tail: String,
}

pub struct VerifyStep {
    pub program: String,
    pub args: Vec<String>,
    pub exit_code: i32,
    pub duration: std::time::Duration,
    pub timed_out: bool,
}
```plaintext

A Change task cannot transition to `Completed` without all required verification
steps passing.

---

# 11. AI Wiki and Evidence Memory

## 11.1 Purpose

The AI Wiki is compiled durable local memory. It is not a raw transcript and not
a store of model hidden reasoning.

Suggested layout:

```text
.ai-wiki/
├── state-of-the-world.md
├── decisions.md
├── verification.md
├── evidence/
│   └── <task-id>.md
├── plans/
│   └── <task-id>.md
├── reviews/
│   └── <task-id>.md
├── sessions/
│   └── <task-id>.jsonl
└── log.md
```plaintext

## 11.2 Context assembly

The orchestrator supplies the model only bounded relevant content:

- task request;
- selected task mode;
- allowed skills;
- selected Wiki sections;
- evidence summaries;
- bounded action results;
- verifier results;
- explicit system rules.

If Jev is configured and available, it may rerank Wiki sections. If Jev is not
available, the harness uses deterministic section selection.

## 11.3 Evidence briefs

Evidence briefs should include:

- task and microtask IDs;
- task mode;
- inspected files;
- bounded excerpts or content hashes;
- Git status/diff summary if relevant;
- confirmed findings;
- hypotheses marked as hypotheses;
- unresolved questions;
- suggested next action;
- timestamp.

They must exclude secrets, raw credentials, unbounded file content, and hidden
reasoning.

## 11.4 Compile behavior

Compilation combines prior durable state with deterministic trace facts:

- files changed;
- action outcomes;
- approvals;
- verifier outcomes;
- Git diff statistics;
- blocked reasons;
- checkpoint commit, if any.

Model-assisted compilation is permitted only as a bounded convenience. The
compiler validates required headings and falls back to a deterministic digest if
model output is invalid.

Writes must use temp-file plus atomic rename.

---

# 12. Jev/System 1 Role

Jev remains optional and advisory.

## 12.1 Permitted uses

- rank AI Wiki sections during Learn;
- classify a no-action model response as completion claim, user question, or stall;
- evaluate whether presented evidence appears to meet the stated goal;
- rerank local retrieval candidates in a future retrieval feature.

## 12.2 Prohibited uses

Jev must not:

- grant permissions;
- authorize mutations;
- override policy;
- select arbitrary scripts;
- mark verification as successful;
- decide that a failed test is acceptable;
- decide task completion independently.

If Jev is unavailable or emits malformed output, the harness uses conservative
fallback behavior.

---

# 13. LLM Integration

## 13.1 Backend trait

The orchestrator depends on a backend trait, not a provider-specific client.

```rust
#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &serde_json::Value,
        event_tx: &tokio::sync::mpsc::UnboundedSender<UiEvent>,
    ) -> crate::error::Result<AssistantTurn>;

    async fn complete_text(
        &self,
        system: &str,
        user: &str,
    ) -> crate::error::Result<String>;
}
```plaintext

## 13.2 Local operating profile

Primary target:

```text
llama.cpp llama-server
http://127.0.0.1:8080/v1/chat/completions
```plaintext

Typical startup:

```bash
llama-server \
  --jinja \
  -m model.gguf \
  -c 32768 \
  --port 8080
```plaintext

Recommended model behavior:

| Phase | Temperature | Tool availability |
| --- | ---: | --- |
| Learn | 0.1 | None |
| Plan | 0.1–0.2 | None |
| Act | 0.1–0.3 | Mode-scoped actions only |
| Verify | N/A | Harness only |
| Review | 0.1 | Optional read-only actions |
| Compile | 0.1 | None |

## 13.3 Streaming rules

The streaming client must:

- preserve only visible `TextDelta` content;
- avoid persisting raw reasoning fields;
- reassemble fragmented tool/action-call deltas;
- coalesce UI output;
- emit one truncation warning if output is dropped;
- treat the fully assembled assistant turn as authoritative;
- fail closed if stream redaction fails.

---

# 14. State Machine and Completion

## 14.1 States

```rust
pub enum LoopState {
    Boot,
    Learn,
    Plan,
    Act,
    Verify,
    Review,
    Compile,
    Completed,
    Blocked,
    Cancelled,
    Failed,
}
```plaintext

## 14.2 Core transitions

```text
Boot     → Learn
Learn    → Plan | Review
Plan     → Act | Review | Blocked
Act      → Act | Verify | Review | Blocked
Verify   → Act | Review | Blocked
Review   → Act | Verify | Compile | Blocked
Compile  → Completed
Any      → Cancelled | Failed
```plaintext

The selected task mode limits which transitions are valid.

## 14.3 Definition of done

A Change task may become `Completed` only when:

1. task and microtask budgets are within limits;
2. no pending approval exists;
3. every mutation has action evidence;
4. required verification ran;
5. every required verification step exited with code `0`;
6. review identifies no known unmet objective;
7. audit and Wiki artifacts were persisted;
8. final reporting distinguishes verified facts from limitations.

A read-only task may complete only once its required evidence brief, plan, or
review artifact has been compiled successfully.

---

# 15. Microtasks and Local Git Checkpoints

## 15.1 Microtask model

Large tasks should be decomposed into small, independently verifiable units.

Each microtask defines:

- identifier;
- bounded goal;
- explicit non-goals;
- permitted files or scope;
- selected mode and skills;
- required verification;
- completion rule;
- expected evidence;
- optional checkpoint policy.

Example:

```text
Microtask ID: change-003
Goal: Add input validation to one parser entry point.
Allowed files:
- src/parser.rs
- tests/parser_tests.rs
Required verification:
- cargo fmt --check
- cargo test parser_tests
Completion:
- verification passes and operator accepts the resulting diff.
```plaintext

## 15.2 Checkpoint process

```text
Select microtask
→ inspect required evidence
→ produce bounded plan
→ approve and apply bounded mutation
→ run required verification
→ inspect local diff and untracked files
→ operator accepts or rejects
→ optionally create local checkpoint commit
```plaintext

## 15.3 Initial Git restrictions

The first release does not expose model-driven Git mutation.

The harness must not initially support:

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
submodule commands
history rewrite
remote modification
```plaintext

A future checkpoint helper may create a local commit only after verification and
explicit operator confirmation.

---

# 16. Security Controls

## 16.1 Mandatory controls

- loopback-only default endpoint profile;
- approved-remote endpoint profile only through explicit configuration;
- no automatic fallback;
- `reqwest` with `default-features = false`;
- root-jail and symlink-escape protection;
- secret-path deny list;
- argv-only child process execution;
- no generic shell execution;
- no `sudo`;
- bounded request, output, stderr, runtime, and context;
- fail-closed approvals;
- deterministic verifier profiles;
- append-only redacted audit evidence;
- no telemetry;
- no inherited proxy, cloud, or model credential variables for action scripts.

## 16.2 Optional hardened profile

The hardened profile may use Bubblewrap or equivalent OS-level containment.

When enabled, it should provide:

- no network route;
- no DNS resolver configuration;
- only loopback interface;
- constrained mounts;
- minimized environment;
- controlled temporary directory;
- process isolation where supported.

If the hardened profile is explicitly required and unavailable, startup must fail
closed.

---

# 17. Project Structure

```text
local-agent-harness/
├── Cargo.toml
├── README.md
├── SECURITY.md
├── HARNESS_PLAN.md
├── REMAINING_TASKS.md
├── REMAINING_TASKS_INDEX.md
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── error.rs
│   ├── events.rs
│   ├── state_machine.rs
│   ├── policy.rs
│   ├── approval.rs
│   ├── audit.rs
│   ├── orchestrator.rs
│   ├── agent/
│   │   └── verify.rs
│   ├── llm/
│   │   ├── mod.rs
│   │   └── openai_compat.rs
│   ├── jev/
│   │   └── client.rs
│   ├── wiki/
│   │   ├── mod.rs
│   │   ├── compile.rs
│   │   └── redact.rs
│   ├── script_runner/
│   │   ├── mod.rs
│   │   ├── protocol.rs
│   │   └── actions.rs
│   ├── skills/
│   │   ├── mod.rs
│   │   ├── investigate.rs
│   │   ├── plan.rs
│   │   ├── change.rs
│   │   └── review.rs
│   ├── tui/
│   │   ├── app.rs
│   │   ├── input.rs
│   │   ├── render.rs
│   │   └── worker.rs
│   └── mcp/                 # Frozen prototype/optional future adapter
│       ├── mod.rs
│       ├── client.rs
│       ├── protocol.rs
│       ├── process.rs
│       └── capability.rs
├── scripts/
│   ├── read_file.py
│   ├── git_status.py
│   ├── git_diff.py
│   ├── write_file.py
│   └── run_verifier.py
├── docs/
│   └── tasks/
├── tests/
└── .ai-wiki/
```plaintext

---

# 18. Active Implementation Roadmap

## Phase A — Document and isolate the architecture transition

1. Add this unified plan.
2. Update architecture and remaining-task documents.
3. Label MCP as frozen prototype/reference work.
4. Ensure the current build continues to compile without requiring MCP runtime
   configuration.

**Acceptance:** documentation consistently describes fixed skills/scripts as the
active execution substrate.

## Phase B — Script runner foundation

1. Add `src/script_runner/`.
2. Define typed request/response protocol.
3. Implement fixed action-to-script mapping.
4. Implement bounded stdin/stdout/stderr.
5. Add timeout, cancellation, and child-process cleanup.
6. Add minimal environment filtering.
7. Add audit metadata for script identity and outcome.

**Acceptance:** mock scripts prove success, malformed protocol handling, timeout,
cancellation, output caps, and no model-controlled executable path.

## Phase C — Shared project-root guard

1. Extract or reuse the existing root/path policy.
2. Add script-compatible request validation.
3. Add existing-path canonicalization.
4. Add new-file parent canonicalization.
5. Enforce sensitive-path denial.
6. Add adversarial traversal and symlink tests.

**Acceptance:** all filesystem action paths are consistently confined before
script dispatch.

## Phase D — Read-only skills

Implement:

```text
read_file
git_status
git_diff
```plaintext

Then implement `Investigate` mode with evidence-brief compilation.

**Acceptance:** an Investigate task can inspect a disposable repository, produce
bounded evidence, and make no mutation.

## Phase E — Planning workflow

Implement Plan mode:

- bounded model-generated plan;
- explicit assumptions;
- expected verifier profile;
- saved plan artifact;
- no mutation.

**Acceptance:** Plan mode produces a durable reviewed plan from repository
evidence without exposing change actions.

## Phase F — Controlled write support

Implement in order:

1. write schema validation;
2. new-file creation only;
3. approval integration;
4. guarded overwrite using expected SHA-256;
5. atomic write cleanup tests;
6. audit and Wiki trace integration.

**Acceptance:** denied, expired, stale, or mismatched approvals cannot cause
writes; expected-SHA mismatch preserves the existing file.

## Phase G — Deterministic verification integration

1. Invoke fixed verifier profiles through `run_verifier.py`.
2. Preserve exit code, duration, timeout status, and bounded output tail.
3. Require verification after mutation.
4. Block unsupported project types or missing dependencies.

**Acceptance:** a task with failed verification cannot complete, even if the
model claims success.

## Phase H — Change-mode end-to-end workflow

Implement:

```text
Learn → Plan → approved write → Verify → Review → Compile
```plaintext

Use a disposable local repository and a local `llama-server`.

**Acceptance:** one bounded mutation is approved, verified, reviewed, audited,
and compiled into the Wiki.

## Phase I — Review mode and checkpoint pilot

1. Implement read-only Review mode.
2. Report changed and untracked files.
3. Add operator-approved local checkpoint workflow design.
4. Run recovery drills against a disposable repository.

**Acceptance:** the operator can inspect evidence and decide whether to create a
small local checkpoint after a verified microtask.

## Phase J — Release and operational packaging

1. Release build/install layout.
2. Artifact checksums and provenance.
3. CI for default and remote feature builds.
4. Optional hardened-profile CI lane.
5. Live local-model and TUI smoke test procedure.
6. Real-repository pilot with recovery review.

---

# 19. Deferred Work

The following are intentionally deferred:

- dynamic MCP discovery;
- arbitrary MCP calls;
- remote MCP servers;
- generic shell execution;
- shell command strings;
- arbitrary program execution;
- arbitrary Python code;
- package installation;
- dependency upgrades;
- generic network access;
- remote Git changes;
- automatic Git commits;
- Git push/fetch/reset/checkout/clean/rebase;
- patches and multi-file refactors beyond reviewed future scope;
- concurrent tasks;
- multi-user/multi-tenant operation;
- automated self-modification;
- separate work-API adapter unless OpenAI compatibility is insufficient.

---

# 20. Known Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Local model produces malformed actions | Strict schemas, teaching errors, small action catalog |
| Context window pressure | Fixed skills, terse schemas, bounded outputs, compiled Wiki |
| Script protocol drift | Versioned request/response schema and mock-script tests |
| Path traversal or symlink escape | Shared canonical path guard and adversarial tests |
| Write race or stale overwrite | Expected SHA-256 and atomic replacement |
| Missing dependencies | Block; do not auto-install in initial release |
| False completion claim | Deterministic verification is the completion gate |
| Wiki hallucination | Deterministic trace, heading validation, fallback digest |
| Git rollback incompleteness | Report untracked files; do not treat Git as sole containment |
| Hardened sandbox unavailable | Fail closed when that profile is explicitly required |
| MCP prototype diverges | Freeze it behind existing interfaces; do not expand it during P0 |

---

# 21. Final Definition of Done

The initial architecture transition is complete when:

1. `HARNESS_PLAN.md` is the single active plan.
2. The architecture documents consistently identify fixed scripts and skills as
   the active execution substrate.
3. MCP remains compilable/reference-preserved but is not required for the P0
   user workflow.
4. A fixed script runner dispatches only canonical reviewed scripts.
5. Read-only filesystem and Git inspection work below one configured root.
6. Mutation requires policy preflight and explicit approval.
7. File overwrite requires expected SHA-256.
8. Mutations require deterministic verification before completion.
9. Evidence, audit, and Wiki compilation remain bounded and redacted.
10. A real local-model pilot demonstrates one verified bounded change in a
    disposable repository.
11. Operator-approved microtask checkpoints are designed and tested before
    enabling any automated commit policy.
