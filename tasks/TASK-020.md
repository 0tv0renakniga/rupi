---
# TASK-020: Implement AI Wiki layout and evidence compilation

**Version:** v1
**Depends on:** TASK-009, TASK-019
**Status:** Not started

## Objective
Add the durable AI Wiki layout and a bounded compilation pipeline that combines deterministic trace facts with optional model summarization and falls back to a deterministic summary on failure.

## Context
Section 17.1 describes the AI Wiki as "compact durable project memory, not a transcript and not a hidden reasoning store." The suggested repository-local layout is `.ai-wiki/` with `state-of-the-world.md`, `evidence/`, `plans/`, `reviews/`, `verification.md`, `log.md`, and `sessions/`. Because it lives in the project root, Wiki content is loaded as untrusted repository data. Section 17.3 requires compilation to validate required headings, validate output size, write atomically, preserve append-only audit/session evidence, and fall back to deterministic summary if model compilation fails. Section 21, Open Question 1 leaves the Wiki location unresolved.
⚠️ Source plan underspecifies this item — verify before starting. The source leaves the AI Wiki location open in Section 21, Open Question 1.

## Scope
- Implement the suggested `.ai-wiki/` layout, subject to resolution of Open Question 1.
- Compile artifacts from deterministic trace facts plus optional bounded model summarization.
- Validate required headings and output size.
- Write compiled output atomically.
- Preserve append-only audit/session evidence.
- Fall back to a deterministic summary when model compilation fails.
- Treat existing Wiki content as untrusted repository data.

## Acceptance Criteria
- The Wiki layout supports state, evidence, plans, reviews, verification, log, and sessions.
- Compilation validates required headings and output size.
- Compiled output is written atomically.
- Append-only audit/session evidence is preserved.
- Model compilation failure falls back to a deterministic summary.
- Wiki content is treated as untrusted repository data.
- The Wiki location decision is confirmed before completion.

## Out of Scope
- Evidence schema and atomic artifact writer (TASK-019).
- Mode-specific lifecycles (TASK-021 through TASK-023).
- Storing hidden reasoning or raw transcripts in the Wiki.
- Unbounded or unredacted content.
---
