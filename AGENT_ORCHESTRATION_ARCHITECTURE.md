# Agent Orchestration Architecture

## Purpose

This document defines the target architecture for `local-agent-harness`: a
local-first Rust task runner for bounded repository work such as:

- understanding a code component;
- planning a feature or remediation;
- resolving a narrowly scoped issue;
- triaging a CVE;
- reviewing a merge request or diff;
- making an approved, verified code change.

The harness has **one security-enforcing orchestrator**. It does not use separate
orchestrators for CVEs, features, Git issues, or code review. Those are
operator-selected task modes and reviewed skills that use the same underlying
state machine, policy boundary, audit system, and verification rules.

This is not a general autonomous-agent platform. It is a local development
harness that operates inside one explicitly configured project root.

---

# 1. Design Principles

## 1.1 One orchestrator, many task modes

The harness owns a single task lifecycle:

```text
Learn → Plan → Act → Verify → Review → Compile → Complete
```plaintext

Not every task uses every phase.

For example:

```text
Investigate:
Learn → Review → Compile → Complete
```plaintext

```text
Plan:
Learn → Plan → Review → Compile → Complete
```plaintext

```text
Change:
Learn → Plan → Act → Verify → Review → Compile → Complete
```plaintext

```text
Review:
Learn → Review → Compile → Complete
```plaintext

The model may propose a next action or completion, but it does not select its own
authority level, tools, scripts, verification commands, or completion state.

The governing rule is:

```text
Model proposes
→ orchestrator checks mode and policy
→ approved fixed action executes
→ deterministic verification proves results
→ orchestrator decides completion
```plaintext

## 1.2 Local-first and fixed-root operation

The first deployment targets one configured project root, for example:

```text
/home/user/dev/project
```plaintext

The root is canonicalized at startup. File operations may use only
repository-relative paths that remain inside that root.

The harness must reject:

- absolute paths;
- `..` traversal;
- NUL bytes;
- paths resolving outside the root;
- symlink escapes;
- configured harness-owned sensitive paths, such as audit and Wiki locations.

The model never supplies:

- a project root;
- a script path;
- an executable path;
- a working directory outside the root;
- a shell command string;
- a network destination.

## 1.3 Fixed skills and helper scripts before MCP

The first usable deployment does **not** require MCP as its execution substrate.

Instead, the Rust orchestrator invokes a small set of fixed, version-controlled
helper scripts. MCP code may remain as a prototype or optional future adapter,
but it is not the P0 critical path.

This reduces complexity:

```text
Fixed skill
→ fixed action identifier
→ fixed local script
→ bounded JSON input/output
```plaintext

rather than:

```text
Model-visible dynamic tool schema
→ external MCP server lifecycle
→ JSON-RPC transport
→ tool discovery and capability negotiation
```plaintext

The simplified approach preserves the important control boundary: the model may
request a known action, but only the orchestrator can authorize and execute it.

## 1.4 Git is a checkpoint mechanism, not the security boundary

Small verified Git commits are useful recovery and audit checkpoints.

Git rollback helps recover tracked files, but it is not sufficient as the only
safety mechanism. For example, `git reset --hard` does not necessarily remove
untracked files, undo external side effects, revoke leaked data, or terminate a
process that has already started.

Therefore the harness still requires:

- fixed project-root restrictions;
- explicit approval before mutation;
- bounded fixed scripts;
- deterministic verification;
- redacted local audit evidence;
- small reviewable commits after accepted microtasks.

---

# 2. System Components

```text
┌───────────────────────────────────────────────────────────────────┐
│                           TUI / CLI                                │
│  task selection · streaming display · approval · deny · cancel     │
└──────────────────────────────┬────────────────────────────────────┘
                               │ typed user commands
┌──────────────────────────────▼────────────────────────────────────┐
│                         ORCHESTRATOR                               │
│  state machine · task mode · policy · approvals · audit · Wiki     │
│  verification gate · microtask checkpoint control                  │
└─────────────┬──────────────────────────────┬──────────────────────┘
              │                              │
              │ model request                │ approved fixed action
┌─────────────▼─────────────┐    ┌───────────▼──────────────────────┐
│ Local LLM / Optional Jev  │    │ Fixed Script Runner               │
│ local OpenAI-compatible   │    │ action ID → canonical script      │
│ endpoint, bounded stream  │    │ argv-only · JSON stdin/stdout     │
└───────────────────────────┘    └───────────┬──────────────────────┘
                                              │
                            ┌─────────────────▼─────────────────┐
                            │ Version-Controlled Helper Scripts │
                            │ read_file.py                      │
                            │ git_status.py                     │
                            │ git_diff.py                       │
                            │ write_file.py                     │
                            │ run_verifier.py                   │
                            └───────────────────────────────────┘
```plaintext

## 2.1 TUI and CLI

The TUI is a **display and control surface only**.

It may:

- collect a task request;
- display bounded visible model text;
- show progress and audit details;
- display a pending approval request;
- submit typed approve, deny, cancel, and quit commands.

It must not:

- execute scripts directly;
- call model endpoints directly;
- decide a task is complete;
- bypass policy or approval checks;
- expose raw reasoning, secrets, unbounded tool input, or raw file content.

## 2.2 Orchestrator

The orchestrator is the trusted task-running component.

It owns:

- state transitions;
- task-mode selection;
- skill selection;
- context assembly;
- policy evaluation;
- approval requests and expiry;
- fixed-action dispatch;
- verification;
- audit writing;
- Wiki compilation;
- completion, blocking, failure, and cancellation.

The orchestrator is the only component that may transition a task to
`Completed`.

## 2.3 Local LLM and optional routing model

The default language-model backend is a local OpenAI-compatible endpoint such as
a loopback `llama-server`.

The LLM is responsible for:

- interpreting the selected task;
- proposing read-only investigation actions;
- drafting plans;
- proposing bounded changes;
- interpreting verification output;
- drafting final reports.

The LLM is not responsible for:

- selecting arbitrary executables;
- granting itself tool access;
- approving mutations;
- choosing verifier commands;
- deciding that verification is unnecessary;
- marking the task complete.

An optional local discriminative router, such as Jev, may provide advisory
routing, scoring, or escalation signals. It is not a source of authority. If it
is unavailable, the orchestrator must fail conservatively rather than silently
fall back to an unapproved endpoint.

---

# 3. Task Modes and Skills

## 3.1 Task modes

A task mode is an **operator-selected workflow and policy bundle**.

| Mode | Typical phases | Mutation authority | Primary output |
| --- | --- | --- | --- |
| Investigate | Learn → Review → Compile | Forbidden | Evidence brief |
| Plan | Learn → Plan → Review → Compile | Forbidden | Reviewed implementation plan |
| Change | Learn → Plan → Act → Verify → Review → Compile | Approval required | Verified change report |
| Review | Learn → Review → Compile | Forbidden | Review findings |

Modes constrain:

- which phases may run;
- which skills are available;
- whether mutation actions may be requested;
- whether approval is required;
- required evidence artifacts;
- terminal and blocked conditions.

A task mode may change only between tasks unless a future design explicitly adds
a safe transition rule. A model may not upgrade itself from `Investigate` to
`Change`.

## 3.2 Skills

A skill is a narrow, reviewed procedure usable within a selected task mode.

Examples:

| Skill | Mode(s) | Authority |
| --- | --- | --- |
| Read source file | Investigate, Plan, Review, Change | Read-only |
| Inspect Git status | Investigate, Plan, Review, Change | Read-only |
| Inspect Git diff | Investigate, Plan, Review, Change | Read-only |
| Produce evidence brief | Investigate, Plan, Review | No repository mutation |
| Create file | Change | Approval required |
| Replace file with SHA guard | Change | Approval required |
| Run deterministic verifier | Change | Harness-selected only |

Skills must be:

- fixed;
- versioned;
- schema-validated;
- operator-allowlisted;
- bounded in input, output, and duration;
- recorded in local audit evidence.

A skill must not:

- be authored by the model at runtime;
- be loaded from an arbitrary path;
- expand the selected mode’s authority;
- add arbitrary tools or scripts;
- perform generic shell execution.

---

# 4. Fixed Script Execution Substrate

## 4.1 Script runner contract

The Rust script runner maps a fixed action identifier to a canonical,
version-controlled script path.

Example action mapping:

```text
ReadFile        → scripts/read_file.py
GitStatus       → scripts/git_status.py
GitDiff         → scripts/git_diff.py
WriteFile       → scripts/write_file.py
RunVerifier     → scripts/run_verifier.py
```plaintext

The model never provides a script path.

The runner invokes scripts through argv only:

```text
python3 /canonical/harness/scripts/read_file.py
```plaintext

It must never construct or execute:

```text
sh -c "..."
bash -c "..."
python3 -c "<model-provided code>"
```plaintext

## 4.2 Script input and output

Scripts receive bounded structured JSON through standard input and return bounded
structured JSON through standard output.

Example request:

```json
{
  "action": "read_file",
  "project_root": "/canonical/project/root",
  "path": "src/lib.rs",
  "line_start": 1,
  "line_end": 80
}
```plaintext

Example response:

```json
{
  "ok": true,
  "path": "src/lib.rs",
  "content": "bounded file content",
  "truncated": false
}
```plaintext

Script diagnostics go to stderr. Protocol output goes to stdout only.

The runner must enforce:

- maximum request size;
- maximum response size;
- maximum stderr size;
- fixed timeout;
- cancellation behavior;
- JSON parsing;
- structured error conversion.

## 4.3 Initial script set

The initial deployment should expose only:

| Script | Purpose | Mutation |
| --- | --- | --- |
| `read_file.py` | Read bounded UTF-8 source text inside the root | No |
| `git_status.py` | Read local Git status | No |
| `git_diff.py` | Read local staged or unstaged Git diff | No |
| `write_file.py` | Create or guarded-overwrite a text file | Yes, approval required |
| `run_verifier.py` | Run harness-selected deterministic checks | No arbitrary command authority |

There is no generic `run_command` or generic shell script in the first
deployment.

## 4.4 Child process environment

Scripts run with a minimal environment.

The runner should:

- set a known working directory below the configured project root;
- pass only required variables such as a controlled `PATH`, `HOME`, and locale;
- avoid inheriting ambient credentials;
- avoid proxy and cloud credential variables;
- avoid passing model credentials to scripts;
- enforce a hard timeout;
- collect bounded stdout and stderr.

An optional OS sandbox, such as Bubblewrap, may be retained as a hardened
deployment profile. It is not required to block the first single-user local
pilot if the root guard and fixed-script model are in force.

---

# 5. Policy and Approval Boundary

## 5.1 Two-layer enforcement

Every action is checked twice.

### Model-facing filtering

The model receives only skill/action descriptions allowed by the selected task
mode.

For example, an `Investigate` task may see:

```text
read_file
git_status
git_diff
```plaintext

A `Change` task may additionally request:

```text
write_file
run_verifier
```plaintext

### Runtime execution veto

Before dispatch, the orchestrator validates:

1. selected mode;
2. selected skill;
3. project-root path policy;
4. action schema;
5. mutation policy;
6. approval state;
7. fixed script mapping;
8. timeout and output bounds.

Any failed check prevents dispatch.

## 5.2 Approval behavior

Mutating actions require an approval request with:

- one unique action ID;
- bounded redacted metadata;
- the intended relative path;
- action type;
- purpose;
- expected SHA-256, where relevant.

The approval order is:

```text
Parse
→ policy preflight
→ fixed-skill/action preflight
→ approval
→ dispatch
```plaintext

Denied, expired, cancelled, stale, or missing approvals never execute the
script.

If no interactive approval owner exists, the harness fails closed and denies the
mutation.

## 5.3 Error handling

Action failures must be represented as structured bounded outcomes. They must
not crash the task loop or bypass audit recording.

The model may receive a bounded corrective message such as:

```text
ACTION_DENIED
PATH_OUTSIDE_PROJECT_ROOT
APPROVAL_EXPIRED
VERIFICATION_FAILED
SCRIPT_TIMEOUT
```plaintext

The model must not receive secrets, raw environment variables, unrestricted
stderr, or hidden implementation details unnecessarily.

---

# 6. Context, Memory, and Evidence

## 6.1 Compiled local Wiki

The harness stores durable local task knowledge in `.ai-wiki/`.

The Wiki contains compact, redacted, structured artifacts rather than raw
conversation transcripts or hidden reasoning.

Possible artifacts include:

```text
.ai-wiki/
├── state-of-the-world.md
├── evidence/
│   ├── task-001.md
│   └── task-002.md
├── plans/
│   └── plan-001.md
├── reviews/
│   └── review-001.md
└── sessions/
    └── session.jsonl
```plaintext

## 6.2 Evidence briefs

An Investigate, Plan, or Review task should produce an evidence brief containing:

- task ID;
- selected mode and skill identifiers;
- files inspected;
- bounded relevant excerpts or content hashes;
- Git status and diff summary when applicable;
- findings;
- uncertainty and missing evidence;
- recommended next action;
- timestamp and local provenance.

It must not contain:

- hidden model reasoning;
- secrets;
- unbounded source dumps;
- raw approval tokens;
- raw model/provider responses.

## 6.3 Compaction and bounded context

The model receives a bounded context assembled from:

- selected task prompt;
- active mode and available skills;
- current project summary;
- relevant evidence briefs;
- bounded recent tool outcomes;
- verification outcomes.

Older or large material is summarized into compiled evidence artifacts rather
than appended indefinitely to the model context.

Raw audit/session records remain local source evidence, while the model sees
only bounded projections.

---

# 7. Deterministic Verification and Completion

## 7.1 Completion is not a model decision

The model may state that work appears complete. That statement is advisory.

For a mutation-capable Change task, the orchestrator requires:

```text
approved mutation
→ action completed
→ deterministic verifier selected by harness
→ verifier passed
→ review/compile completed
→ task may become Completed
```plaintext

If verification fails:

```text
task state = Blocked or return to Act
```plaintext

The harness must not report success merely because the model produced a final
answer.

## 7.2 Fixed verification profiles

Verification is selected by the harness from project evidence and
operator-configured profiles.

Initial examples:

| Project type | Fixed verification |
| --- | --- |
| Rust | `cargo fmt --check`, `cargo check --all-targets`, `cargo test` |
| Python | `pytest -q` |
| Node | configured `npm run test`, `npm run build`, `npm run typecheck` |

The model cannot invent, replace, widen, or disable verification commands.

Unsupported or ambiguous repositories become `Blocked` until an operator adds a
reviewed verifier profile.

## 7.3 Read-only task completion

Read-only modes do not require build/test verification merely to complete.
Instead, they require their expected evidence artifact:

| Mode | Completion evidence |
| --- | --- |
| Investigate | Compiled evidence brief |
| Plan | Reviewed plan |
| Review | Compiled review findings |
| Change | Passing deterministic verification plus verified change report |

---

# 8. Microtask Execution and Git Checkpoints

## 8.1 Purpose

Complex work should be divided into small independently verifiable microtasks.

A microtask has:

- one bounded goal;
- explicit non-goals;
- allowed files or action scope;
- required checks;
- a clear completion rule;
- one reviewable diff;
- one local checkpoint commit after acceptance.

Example:

```text
Microtask ID: change-003
Goal: Add input validation for a single parser function.
Allowed files:
- src/parser.rs
- tests/parser_tests.rs
Required verification:
- cargo fmt --check
- cargo test parser_tests
Completion:
- verifier passed and operator accepted
```plaintext

## 8.2 Microtask lifecycle

```text
Select mode and skill
→ Learn only what the microtask requires
→ Produce bounded plan
→ Perform one approved change
→ Run deterministic verification
→ Review diff and evidence
→ Operator accepts or rejects
→ Create local Git checkpoint commit
→ Continue or stop
```plaintext

Microtasking is initially an operator workflow. It may become a first-class
orchestrator feature only after real local pilots prove the basic execution path.

## 8.3 Commit checkpoints

A local Git checkpoint commit may be created only after:

1. the required deterministic checks pass;
2. the diff stays within the selected mode and skill authority;
3. the changed files are understood;
4. the operator explicitly approves the checkpoint, unless a future trusted
   auto-checkpoint policy is enabled.

The harness must not automatically commit arbitrary model work by default.

Commit metadata should record:

- task ID;
- microtask ID;
- mode and skill ID;
- verifier status;
- changed-file count;
- resulting commit hash.

The harness must not initially support:

- push;
- fetch;
- reset;
- checkout;
- clean;
- rebase;
- remote modification;
- submodule operations;
- history rewriting.

## 8.4 Untracked files

Untracked files require special attention because a Git reset does not necessarily
remove them.

Before a checkpoint commit, the orchestrator should report:

- modified tracked files;
- added tracked files;
- untracked files;
- deleted files.

The operator must explicitly review unexpected untracked output before accepting
a checkpoint.

---

# 9. Audit and Governance

## 9.1 Local audit

The harness writes append-only bounded local audit records.

Audit entries should include:

- task ID;
- microtask ID when applicable;
- selected mode;
- selected skill;
- action type;
- bounded redacted parameters;
- approval outcome;
- script path identity or script version;
- verification result;
- task transition;
- checkpoint commit hash, if one was created.

Audit entries must not contain:

- secrets;
- bearer tokens;
- credential references;
- hidden reasoning;
- raw large source files;
- unbounded stdout/stderr;
- raw model request or response bodies.

## 9.2 Retention and permissions

The audit directory should be local and operator-readable only.

Target Unix defaults:

```text
audit directory: 0700
audit file:      0600
```plaintext

Rotation should occur only between tasks, never in the middle of a task’s
evidence trail.

## 9.3 Optional hardened execution profile

A future hardened profile may add OS-level process containment, such as
Bubblewrap.

That profile may:

- isolate network access;
- minimize mounts;
- clear child environments;
- create controlled temporary directories;
- restrict process visibility.

It should remain optional for the initial single-user local deployment, but its
fail-closed behavior should be preserved when explicitly configured.

---

# 10. Deferred Capabilities

The following require a new product decision and security review before
implementation:

- dynamic MCP discovery;
- arbitrary MCP tool dispatch;
- remote MCP servers;
- model-selected scripts or executables;
- generic shell command execution;
- shell command strings;
- arbitrary Python execution;
- package installation;
- dependency updates;
- generic network access;
- Git commit/push/fetch/reset/checkout/clean/rebase support beyond reviewed
  local checkpoint commits;
- submodule operations;
- multi-user or multi-tenant operation;
- concurrent independent tasks;
- automatic remote work-API fallback;
- a separate remote/work-API client when the current OpenAI-compatible client
  remains protocol-compatible.

---

# 11. Implementation Priority

The first usable local deployment should proceed in this order:

```text
1. Document script-substrate architecture decision.
2. Implement fixed Rust script runner.
3. Implement shared project-root/path guard.
4. Implement read-only scripts:
   - read_file
   - git_status
   - git_diff
5. Implement Investigate skill.
6. Implement Plan skill using evidence briefs.
7. Integrate existing approval boundary with fixed write action.
8. Implement write_file for new files only.
9. Add guarded overwrite with expected SHA-256.
10. Implement deterministic verifier runner.
11. Implement Change end-to-end flow.
12. Implement read-only Review skill.
13. Run live local-model and TUI pilots.
14. Run disposable real-repository pilot and recovery drills.
15. Add release packaging, provenance, and CI.
16. Consider first-class task-mode and microtask runtime support.
```plaintext

The first implementation goal is not broad autonomy. It is one reliable,
understandable local workflow:

```text
Investigate a repository
→ create a plan
→ approve one bounded change
→ verify it deterministically
→ review the diff
→ save evidence and an optional local checkpoint
```plaintext


