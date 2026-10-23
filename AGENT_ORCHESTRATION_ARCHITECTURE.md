Here is a generalized, highly-collapsed synthesis of the architectural patterns extracted from the multi-repository report. By abstracting away repository-specific naming, we can map these findings directly into **core systems orchestration paradigms** suitable for designing a production-grade Rust AI agent harness.

---

# Generalized Agent Orchestration Architecture

## 1. The Core Orchestration Engine: Declarative Operation Pipelines over Immutable Event Logs

Rather than monolithic `while` loops or mutable state blocks, advanced harnesses rely on a **decoupled operation pipeline** driven by an immutable event stream.

- **State as an Append-Only Log:** All core states (phases, tool approvals, compaction locks) are derived dynamically by folding or replaying an append-only event log, eliminating hidden mutable state bugs.


- **Decoupling Decision from Mutation:** Control flow is handled by an ordered sequence of discrete *Operations* (or steps) that inspect the conversation tail and yield pure data *Effects* (e.g., compaction requests, tool dispatches, phase transitions). The state machine or runner acts purely as a deterministic reducer applying these effects.


- **First-Class Lifecycle and Modes:** Modes dictate the allowable path of phases (e.g., `Investigate`, `Plan`, `Change`), restricting mutation rights and capability access strictly based on the current phase context.



---

## 2. The Capability Layer: Scoped, Layered Registries with Double Enforcement

Tool registries manage execution boundaries, dynamic injection, and security filtering.

- **RAII Scoping and Disposers:** Tool registrations are managed via scoped layers (global vs. session/scope-local) that return unregister/disposer closures, ensuring clean lifecycle management.


- **Presentation vs. Execution Separation:** UI rendering details (`presentCall`, `presentResult`) are strictly decoupled from raw execution payloads (`ToolDefinition`), keeping model context free of UI strings.


- **Double Enforcement Pipeline:** Capability security relies on a two-step gate:
1. *Model-Facing Filtering:* The model is only told about tools permitted by the current mode/phase.


2. *Runtime Execution Veto:* A hard pre-execution gate intercepts any unauthorized call, ensuring that even if a model hallucinates or attempts an unadvertised action, it is blocked.




- **Never-Throw Boundary:** Tool execution failures (validation errors, unknown tools, blocked permissions) never throw across boundaries; they are cleanly wrapped as `isError: true` responses so the conversation transcript remains well-formed and auditable.



---

## 3. The Security & Governance Perimeter: Fail-Closed Defense-in-Depth

Guardrails, approvals, and sandboxing must be enforced structurally rather than treated as optional plugin checks.

- **Fail-Closed Defaults:** If an interactive user interface or explicit approval policy owner is missing, permission requests default instantly to *Deny*.


- **Persisted Approval States:** Approval outcomes (`AllowOnce`, `AlwaysAllow`, `Deny`, etc.) are recorded directly as data inside the conversation log, making security decisions fully replayable and exportable.


- **Sandbox Policy Ownership:** Mutating or executing tools (`Exec`/`Mutate`) require a standing sandbox policy owner before dispatching, forbidding unconfigured or loosely sandboxed runs.



---

## 4. The Context Continuum: Durable Compaction & Evidence Briefs

Long-running sessions manage token limits through structured, crash-resilient memory management.

- **Durable Locking:** State compaction uses explicit durable log events (`CompactionStarted` / `CompactionEnded`) instead of fragile in-memory mutexes, allowing clean recovery if a process crashes mid-compaction.


- **Shadowing over Deletion:** Summarization replaces historical ranges using "shadow" references and checkpoint sources, preserving the raw underlying event log while providing a compacted view to the model.


- **Evidence Briefs with Provenance:** Sub-contexts (like references to other sessions or files) are projected under strict byte budgets, explicitly tracking omitted bytes, truncation notices, and spill locators.



---

## 5. The Completion & Verification Framework: Deterministic Gates

Task completion is treated as a privilege requested by the model but granted exclusively by deterministic harness checks.

- **Model-Asserted Requests vs. Structural Enforcements:** The model can signal completion or stop requests, but the harness forces them through a verification gate (such as execution checks, build/test validation, or audit trails).


- **Vetoable Stop Hooks:** Stopping criteria are validated by out-of-band policies and verifiers; if a verifier fails, the harness injects a corrective context message and forces the loop to continue rather than prematurely terminating.
