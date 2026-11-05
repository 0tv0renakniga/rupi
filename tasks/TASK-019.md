---
# TASK-019: Implement bounded durable evidence artifact writing

**Version:** v1
**Depends on:** TASK-009, TASK-018
**Status:** Not started

## Objective
Create a bounded, redacted, attributable evidence artifact writer so read-only task modes can record durable facts rather than relying on transcript history or hidden reasoning.

## Context
Build Order Phase 3, Step 12: "Implement durable evidence, plan, and review artifacts." Section 17.2 says artifacts should include only bounded, redacted, attributable facts: task ID; mode; selected skill; inspected paths; bounded excerpts or hashes; tool/action summaries; verification results; blockers; assumptions; known limitations. It forbids hidden model reasoning, secrets, raw provider payloads, and unbounded file content. Section 17.3 requires compilation to validate required headings, validate output size, write atomically, preserve append-only audit/session evidence, and fall back to deterministic summary if model compilation fails.

## Scope
- Define an artifact schema for task ID, mode, selected skill, inspected paths, bounded excerpts or hashes, tool/action summaries, verification results, blockers, assumptions, and known limitations.
- Redact secrets and exclude hidden reasoning, raw provider payloads, and unbounded content.
- Bound artifact size.
- Write artifacts atomically.
- Preserve append-only session/audit evidence.
- Route artifact paths through the shared policy.

## Acceptance Criteria
- Evidence artifacts contain only the bounded, redacted, attributable fields listed in Section 17.2.
- Hidden reasoning, secrets, raw provider payloads, and unbounded file content are excluded.
- Artifact size is bounded.
- Writes are atomic.
- Session/audit evidence remains append-only.
- Artifact paths comply with the shared policy.

## Out of Scope
- Wiki compilation and model-assisted summarization (TASK-020).
- Investigate, Plan, and Review mode lifecycles (TASK-021 through TASK-023).
- Mutation evidence.
- Unbounded source excerpts.
---
