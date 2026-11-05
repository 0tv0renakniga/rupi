---
# TASK-008: Load global trusted context with provenance

**Version:** v1
**Depends on:** TASK-000, TASK-001
**Status:** Not started

## Objective
Load operator-owned global context only from the trusted configuration directory and show its provenance in the Context tab so trusted instructions are distinguishable from repository content.

## Context
Build Order Phase 2, Step 7: "Implement trusted operator-owned context loading and Context-tab provenance." Section 10.1 states that global trusted context is loaded only from the trusted configuration directory, with examples including `AGENTS.md`, `CLAUDE.md`, and `prompts/*.md`. It may define operator preferences, organizational conventions, trusted prompt templates, local model defaults, and workflow defaults. It cannot be modified by the model or target repository. Section 6.3 states: "The target repository is never scanned for trusted configuration."

## Scope
- Load global context only from the trusted operator configuration directory.
- Support trusted `AGENTS.md`, `CLAUDE.md`, and `prompts/*.md` sources.
- Display loaded global context and source provenance in the Context tab.
- Keep global context immutable from model or repository influence.
- Bound loaded content according to configuration limits.

## Acceptance Criteria
- Global context is loaded only from the trusted configuration directory.
- The target repository is not scanned for trusted configuration.
- Context-tab provenance identifies each loaded global source.
- Model and repository content cannot modify global trusted context.
- Loaded content is bounded.

## Out of Scope
- Project untrusted context loading and wrappers (TASK-010).
- Slash prompt execution (TASK-011).
- Prompt-template selection and context assembly.
- Remote or repository-provided global instructions.
---
