# Autoresearch, Software 3.0, LLM-Wiki, and Harness²: Fit and Integration Design

> **Status:** Design only. No runtime behavior is changed by this document.
> **Scope:** Explain how the Autoresearch / Software 3.0 / LLM-Wiki /
> GEPA / Harness² framing relates to `local-agent-harness`.
> **Audience:** Maintainers and operators who need to decide what to adopt,
> adapt, or reject.
> **Related source material:** The complete user-provided source text is
> reproduced in [Appendix A](#appendix-a-source-material-verbatim).

---

## 1. Executive summary

Most of the substrate described by the framing already exists in this project,
but in a deliberately narrower and safer form:

```text
bounded task execution
→ read-only or approved mutation
→ deterministic verification
→ bounded audit evidence
→ compiled Wiki memory
→ harness-owned completion
```

This project already implements:

- bounded time and action budgets;
- a fixed, versioned tool/script substrate;
- approval-gated mutation;
- deterministic verifier selection and execution;
- append-only, redacted audit evidence;
- a bounded, redacted AI Wiki with evidence and plan artifacts;
- project instructions such as `AGENTS.md` and `CLAUDE.md` loaded as untrusted
  data;
- a single Rust orchestrator that owns policy, verification, and completion.

The project intentionally does **not** implement:

- autonomous editing of prompts, policies, skills, verifiers, or source code;
- automatic Git commits based on model work;
- automatic `git reset --hard` rollback;
- model-selected executables, script paths, shell command strings, or arbitrary
  Python execution;
- repository content as authority.

Therefore the best fit is:

| Layer | Verdict | Reason |
| --- | --- | --- |
| Time-boxed, iterative experiments | Adapt | Budgets exist, but one task is not the same as repeated experiments. |
| Direct script edits and execution | Reject as written; adapt as microtasks | The model may propose typed actions, not arbitrary code execution. |
| Git as the experiment state machine | Reject automatic commit/reset; keep as operator checkpoint | Git mutation is disabled and reset is explicitly out of scope. |
| Metric decides keep/revert | Adapt for offline experiments | Completion uses a deterministic verifier, not a scalar model metric. |
| Software 3.0 spec + eval + diff review | Already fits | The harness already separates proposal, authority, verification, and review. |
| CLI-native tool use | Already fits with restrictions | Fixed scripts and verifier programs are used, but not a generic shell. |
| LLM-Wiki / externalized memory | Already fits with a trust boundary | The Wiki is durable data, not self-modifying instruction authority. |
| `/audits/` execution transcripts | Already fits | `.harness-audit/audit.jsonl` is append-only, bounded, and redacted. |
| Session audit / GEPA reflective loop | Missing; could be offline only | No runtime mechanism should rewrite prompts from traces. |
| Harness² self-hosting evolution | Reject as runtime self-modification | Explicitly forbidden by the project's non-goals. |

In short: the framing is useful as vocabulary for an **offline, operator-approved
meta-optimization workflow**, not as an autonomous runtime that modifies the
harness or the repository.

---

## 2. Current project mapping

This section maps each idea from the source material to an existing component.

### 2.1 Autoresearch concepts

| Source idea | Existing project equivalent | Classification |
| --- | --- | --- |
| Single-GPU, minimal experiment driver | Single-project, single-harness-process design in `HARNESS_PLAN.md`; no distributed orchestration. | Partial fit |
| A short fixed time budget per iteration | `TaskBudget` in `src/state_machine.rs:146` with `max_act_turns`, `max_tool_calls`, `max_verify_retries`, `max_system2_calls`, `max_wall_time`, and `max_tool_output_bytes`. Wall time is configurable with `HARNESS_MAX_WALL_SECS` in `src/config.rs:435` (default 3600 seconds). | Adapt: set a short wall-clock budget for experiment runs |
| The agent reads and edits `train.py` | The model proposes typed actions. Change mode uses fixed actions such as `write_file`. There is no arbitrary script path or generic command. | Reject as written; safe equivalent exists |
| The agent executes the edited script | Fixed script runner maps `ActionId` to canonical helper scripts (`src/script_runner/actions.rs:13`). Verifier commands are harness-selected and argv-only. | Reject generic execution; verifier path fits |
| `git commit` when the metric improves | `Policy.allow_git_commit` is `false` (`src/policy.rs:68`). Local checkpoint commits are an operator-approved future workflow, not an automatic model action. | Conflict |
| `git reset --hard` when the metric degrades or the script crashes | Git reset, checkout, clean, and related mutations are non-goals (`HARNESS_PLAN.md:60`). Git is explicitly not the security boundary (`AGENT_ORCHESTRATION_ARCHITECTURE.md:130`). | Conflict |
| Failure recovery by Git rollback | Failure is represented as `Blocked` or returns to `Act`, with audit evidence and bounded retries. Untracked files and external side effects mean Git rollback is insufficient. | Adapt: use task state and evidence, not reset |
| Zero-overhead, transparent loop | The harness is deliberately explicit: fixed state machine, audit events, Wiki compilation, and verifier gates. | Partial fit |

### 2.2 Software 3.0 concepts

| Source idea | Existing project equivalent | Classification |
| --- | --- | --- |
| Prompt plus context acts as the program execution environment | `src/llm/prompts.rs` defines mode contracts and the untrusted-content invariant. `src/orchestrator.rs:2511` builds the system prompt. `src/orchestrator.rs:2548` builds bounded context. | Fit |
| The spec defines success | Task objective, selected task mode, mode contracts, plan contract, and completion rules. | Fit |
| The eval engine defines verification bounds | `src/agent/verify.rs:156` detects Rust, Python, and Node verification profiles. Verification is harness-selected and deterministic. | Fit, but binary pass/fail rather than scalar metrics |
| Diff review verifies maintainability and alignment | Review phase, evidence compilation, plan compilation, and operator review. | Fit |
| The engineer is a meta-architect | The Rust orchestrator is the authority boundary; the model proposes actions only. | Fit |
| Auto-evolved prompts or code remain maintainable | No autonomous prompt evolution exists. Prompt changes are code or trusted-configuration changes reviewed by a human. | Missing by design |

### 2.3 CLI-native integration

| Source idea | Existing project equivalent | Classification |
| --- | --- | --- |
| Terminal CLIs are ideal agent interfaces | `cargo`, `npm`, and `pytest` are invoked through the harness verifier. Fixed helper scripts run through argv-only invocation. | Fit with restrictions |
| `git` as a CLI tool | Only read-only `git_status` and `git_diff` are exposed through fixed scripts. Git mutation is not available. | Partial fit |
| `pytest` / `python train.py` as agent tools | Python verification is detected as `python3 -m pytest -q`. Arbitrary training scripts are not exposed. | Partial fit |
| Custom CLI dashboards | The harness has a TUI and a `--headless` preflight mode. | Fit |
| Shell pipelines chaining tools | Not exposed. The model never supplies a shell command string. | Reject |
| CLI as the native toolset without API wrappers | Mostly true: the active execution path uses fixed local scripts and local verifier programs. MCP remains optional/frozen, not the critical path. | Fit |

### 2.4 Externalized memory and the LLM-Wiki

| Source idea | Existing project equivalent | Classification |
| --- | --- | --- |
| `CLAUDE.md` / `AGENTS.md` stores conventions and strategies | `src/context.rs:252` loads `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `README.md`, and `.ai-wiki/state-of-the-world.md` as untrusted project context. | Fit as data, not authority |
| The agent reads `AGENTS.md` to recover prior learnings | The harness can load the file into bounded context. It wraps content in `<ProjectInstructions untrusted="true">` and escapes markup. | Fit with trust boundary |
| The agent appends learnings to `AGENTS.md` after success | No automatic write. `AGENTS.md` is repository content and is untrusted. Any edit must go through normal Change mode, approval, and verification. | Conflict with automatic loop |
| `/audits/` stores execution transcripts and failure analyses | `.harness-audit/audit.jsonl` stores append-only, bounded, redacted audit records. Session and Wiki artifacts are stored separately. | Fit |
| The Wiki is compact durable project memory | `.ai-wiki/` contains `state-of-the-world.md`, `log.md`, `sessions/`, `evidence/`, and `plans/` (`src/wiki/mod.rs:20`). | Fit |
| The Wiki is a hidden reasoning store or raw transcript | Explicitly rejected. Only bounded, redacted, attributable facts are persisted. Hidden reasoning is not stored. | Reject |
| Only selected Wiki content enters the prompt | Only `.ai-wiki/state-of-the-world.md` is eligible for model context. Session records and `log.md` are durable local history but are not injected automatically. | Fit |
| Session amnesia is fixed by re-reading memory files | Partially addressed by bounded Wiki selection and context compaction. A reflective prompt-update loop does not exist. | Partial fit |

### 2.5 Session audit, GEPA, and Harness²

| Source idea | Existing project equivalent | Classification |
| --- | --- | --- |
| Session audit reads raw execution logs | Audit records exist, but there is no dedicated audit-analysis skill or CLI. | Missing |
| The audit identifies instruction drift or tool-schema misuse | Policy denials, blockers, verifier failures, and action summaries are recorded. No classifier currently derives these failure categories. | Missing |
| GEPA-style reflective prompt evolution | No runtime prompt evolution. Prompts are code-owned and reviewed. | Reject as runtime behavior |
| Harness² short execution probes | The verifier and script runner can run bounded probes, but there is no experiment driver that compares candidate harness variants. | Missing; offline-only fit |
| Compose and commit improvements automatically | No automatic commit. Completion requires verifier success and Wiki/evidence compilation; checkpoints require operator approval. | Conflict |
| Revert and log failure learnings automatically | No automatic revert. Failure is recorded in audit and Wiki artifacts, and the task becomes `Blocked` or retries. | Conflict |

---

## 3. Security invariants and why the autonomous loop does not fit

The source framing describes this loop:

```text
read AGENTS.md and traces
→ edit prompts or routing logic
→ run a short experiment
→ audit the trace
→ commit on improvement, reset on regression
→ append learnings to AGENTS.md
```

The project's governing rules are different:

```text
model proposes
→ Rust validates policy, mode, and task authority
→ fixed action executes
→ bounded evidence returns
→ deterministic verification decides completion
```

The critical constraints are:

1. **Repository content is untrusted.**
   `AGENTS.md`, `CLAUDE.md`, source comments, test output, Git messages, and
   generated files may describe conventions, but cannot override policy,
   approvals, verification, tool availability, endpoint policy, or completion
   rules.

2. **Model output is not authority.**
   The model cannot select its own mode, tools, scripts, executable paths,
   verifier commands, approval state, or completion state.

3. **Git is a checkpoint mechanism, not a security boundary.**
   `git reset --hard` does not necessarily remove untracked files, undo
   external side effects, revoke exposed data, or terminate a process.

4. **Git mutation is disabled by default.**
   `allow_git_commit` is `false`. Push, fetch, pull, reset, checkout, clean,
   rebase, merge, tag, and submodule operations are non-goals.

5. **Autonomous self-modification is a non-goal.**
   The project explicitly excludes autonomous policy, skill, verifier, and
   source-code self-modification.

6. **Completion is harness-owned.**
   A model claim of completion is advisory. For Change mode, the harness
   requires an approved mutation, a passing deterministic verifier, review, and
   compilation.

7. **Wiki and audit artifacts are bounded and redacted.**
   Raw reasoning, secrets, unbounded source dumps, and raw provider payloads
   are not persisted.

Because of these constraints, the autonomous evolution loop cannot be added to
the runtime without changing the project's security model. That would require a
new product decision and security review, not merely a new module.

---

## 4. Safe adaptation design

The useful parts of the framing can be preserved as an offline,
operator-approved workflow that reuses the harness instead of bypassing it.

### 4.1 Offline experiment runner

A separate experiment driver, not the runtime orchestrator, could:

1. Copy a project or fixture into a disposable workspace.
2. Accept a bounded candidate change, such as a prompt variant, configuration
   variant, or fixture-specific code change.
3. Run a fixed number of iterations with a short wall-clock budget.
4. Execute the harness-owned verifier or an allowlisted metric command.
5. Record structured results, including verifier output, durations, turn counts,
   tool calls, retries, denials, and task state.
6. Produce a ranked report and candidate diff.

The experiment driver must not:

- run against the live project root by default;
- accept arbitrary shell strings;
- perform automatic commits or resets;
- modify `AGENTS.md`, policy, skills, verifiers, or source code without a human
  applying the result through normal Change mode;
- bypass approval, path policy, output bounds, or redaction.

### 4.2 Read-only session audit analyzer

A separate CLI could parse `.harness-audit/audit.jsonl` and produce a bounded
summary of:

- budget exhaustion by dimension;
- policy denials;
- verifier failures and retries;
- tool-schema misuse;
- repeated stalls or empty model outputs;
- approval denials or expiry;
- Wiki or evidence compilation failures;
- task-state transitions.

The analyzer should emit a report for a human. It should not rewrite prompts or
instructions automatically.

### 4.3 Proposal-based promotion

Instead of self-modifying the harness:

1. Store candidate prompt or configuration changes as proposals.
2. Attach experiment evidence and the bounded audit summary.
3. Require human review.
4. Apply accepted proposals through normal Change mode, with approval,
   verification, review, and optional operator checkpoint.

This keeps the feedback loop real while preserving the authority boundary.

### 4.4 Metrics

The existing verifier is binary (pass/fail), not a scalar experiment metric.
Offline experiments may additionally record:

- verification pass rate;
- wall-clock duration;
- time-box overruns;
- act turns;
- tool calls;
- verification retries;
- policy denials;
- approval outcomes;
- task state;
- evidence and audit sizes.

Any scalar benchmark metric should remain an offline comparison signal. It must
not replace the deterministic completion gate for Change mode.

### 4.5 Reuse map

| Capability | Reusable module |
| --- | --- |
| Bounded task execution | `src/state_machine.rs` |
| Audit records | `src/audit.rs` |
| Redaction | `src/wiki/redact.rs` |
| Session trace | `src/wiki/compile.rs` |
| Evidence compilation | `src/wiki/evidence.rs` |
| Plan compilation | `src/wiki/plan.rs` |
| Deterministic verification | `src/agent/verify.rs` |
| Script execution | `src/script_runner/` |
| Path policy | `src/path_policy.rs`, `src/policy.rs` |
| Orchestration | `src/orchestrator.rs` |

---

## 5. Proposed backlog items

These are suggestions only and are not implemented by this document.

| Proposed item | Purpose |
| --- | --- |
| TASK-039: Offline bounded experiment runner | Compare candidate variants in disposable workspaces using existing verification and bounded budgets. |
| TASK-040: Read-only audit analyzer | Summarize audit traces into failure and non-progress categories for human review. |
| TASK-041: Experiment evidence format | Define a bounded, redacted result schema and storage location for offline experiments. |
| TASK-042: Proposal review workflow | Store candidate prompt/config changes as proposals with evidence, requiring operator promotion. |

---

## 6. Open questions

1. Should experiment results live under `target/experiments/`, `.harness-audit/`,
   or a new operator-owned data directory?
2. Is scalar benchmark support wanted, or is verifier pass/fail sufficient for
   the first experiment runner?
3. Should proposal review become a first-class mode, or remain an operator-only
   workflow outside the harness?
4. Should `AGENTS.md` eventually contain operator-authored, trusted
   meta-guidance, or should all repository-authored content remain strictly
   untrusted?
5. Does the project want an explicit architecture decision that permits offline
   meta-optimization while continuing to forbid runtime self-modification?

---

## Appendix A: Source material (verbatim)

The following is the complete material supplied by the user, preserved so the
analysis above can be checked against the original framing.

> i am want to know how this following fits into the project:

### 1. What Autoresearch Actually Is (and Why It's Unique)

karpathy/autoresearch isn't just an abstract idea—it is a hyper-focused, minimal (~630 lines of PyTorch/Python) autonomous experiment driver designed around single-GPU constraints.

```text
                  ┌─────────────────────────────────────────┐
                  │          karpathy/autoresearch          │
                  │   Single GPU • ~630 Lines • PyTorch     │
                  └────────────────────┬────────────────────┘
                                       │
            ┌──────────────────────────┴──────────────────────────┐
            ▼                                                     ▼
┌─────────────────────────┐                             ┌───────────────────┐
│     TIME-BOXED RUNS     │                             │   GIT-BASED DIFF  │
│   ~5 min per iteration  │                             │   Keep if metric ↑ │
│   Prevents runaways     │                             │   Revert if metric ↓│
└─────────────────────────┘                             └───────────────────┘
```

**The 5-Minute Time-Box:** Rather than letting an agent run open-ended, expensive training jobs, Autoresearch constrains each experiment to a short, fixed time budget (e.g., 5 minutes on a single GPU). This forces high iteration speed and makes validation metrics immediately comparable across runs.

**Direct Script Edits via Diffing:** The agent reads train.py, modifies hyperparameters, model architecture, or loss functions directly, and executes the script via CLI.

**Git as the State Machine:**

- Metric Improves? `git commit` with a brief summary of the hypothesis and gain.
- Metric Degrades / Script Crashes? `git reset --hard` to cleanly undo the change.

**Zero Overhead:** Because it is only ~630 lines of code, the loop is completely transparent—no complex multi-agent orchestration frameworks or heavy abstractions getting in the way.

### 2. Agentic Engineering & "Software 3.0"

Your point on Software 3.0 frames how we build software when LLMs are the runtime:

**Prompt + Context = Interpreter:** In Software 1.0 (code) and 2.0 (neural net weights), humans write logic or train tensors. In Software 3.0, the context window, prompt spec, and available tool definitions act as the program execution environment.

**The Engineer's New Role:** Instead of writing every function or manually tweaking prompts, the human operates as a meta-architect who designs three core components:

- **The Spec:** Defining what success looks like (AGENTS.md, task descriptions).
- **The Eval Engine:** Setting up light, robust verification bounds (validation metrics, execution traces).
- **Diff Review:** Inspecting the agent's Git commits to verify that auto-evolved prompts or code remain maintainable and aligned.

### 3. How CLI Native Integration & LLM-Wiki Drive This

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                           LOCAL SYSTEM / REPOSITORY                         │
│                                                                             │
│  ┌───────────────────────┐   CLI Tools    ┌──────────────────────────────┐  │
│  │      LLM-WIKI         │ ────────────>  │        CLI TOOLKIT           │  │
│  │                       │                │                              │  │
│  │  • CLAUDE.md          │  Logs & Traces │  • git (commit/revert)       │  │
│  │  • AGENTS.md          │ <────────────  │  • pytest / python train.py  │  │
│  │  • /audits/*.md       │                │  • custom CLI dashboards     │  │
│  └───────────────────────┘                └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**CLI as the Native Toolset**

Karpathy emphasizes that terminal CLIs (e.g., git, python, pytest, gh) are ideal agent interfaces. They don't require custom API wrappers, they chain naturally via shell pipelines, and they give agents direct access to the standard developer toolkit.

**Externalized Memory (LLM-Wiki / CLAUDE.md)**

Context windows drop past detail over long sessions ("session amnesia"). Externalized memory fixes this by writing knowledge directly into markdown files in the repository:

- **CLAUDE.md / AGENTS.md:** Stores project conventions, tool schemas, known edge cases, and successful prompt strategies.
- **/audits/:** Stores execution transcripts and failure analyses.

When an agent starts a new iteration, it reads AGENTS.md to load its past learnings without needing thousands of tokens of raw chat history.

### 4. Connecting All 3 Systems

Here is how Autoresearch, Session Audits, and GEPA / Harness² connect line-by-line:

```text
┌──────────────────────────────────────────────────────────────────────────────────┐
│                         AUTONOMOUS EVOLUTION LOOP                               │
├──────────────────────────────────────────────────────────────────────────────────┤
│                                                                                  │
│ 1. READ STATE                                                                    │
│    Agent reads AGENTS.md + system prompts + latest audit summaries.             │
│                                                                                  │
│ 2. PROPOSE (Autoresearch style)                                                 │
│    Agent edits prompt files or routing logic via diffs.                         │
│                                                                                  │
│ 3. PROBE (Harness² style)                                                       │
│    Runs short CLI test execution (e.g., 5-min run).                             │
│    Outputs raw execution traces (tool arguments, errors, outputs) to log file.   │
│                                                                                  │
│ 4. SESSION AUDIT (GEPA reflective loop)                                          │
│    Session Audit Skill reads execution log via CLI parser.                      │
│    Identifies non-obvious failures (instruction drift, tool schema misuse).     │
│                                                                                  │
│ 5. COMPOSE & COMMIT                                                              │
│    • IF IMPROVED:                                                                │
│        - Commit diff via `git commit`                                            │
│        - Append extracted learnings to AGENTS.md                                 │
│    • IF DEGRADED / FAILED:                                                       │
│        - Revert via `git reset --hard`                                           │
│        - Log failure cause in AGENTS.md so future runs avoid the mistake         │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────┘
```

### What Would You Like to Dive Into Next?

We can drill down into any specific piece without skipping the implementation details:

- **Autoresearch Setup:** Look at how to structure a tiny ~600-line Python harness script for running prompt/harness experiments.
- **Session Audit Engine:** Look at how a session audit skill parses raw execution logs (JSON/text) to detect instruction drift or tool errors.
- **LLM-Wiki Design:** Look at how to format AGENTS.md so an agent can write to and read from it without corrupting its own instructions over time.
