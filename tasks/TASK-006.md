---
# TASK-006: Implement session JSONL persistence and recovery

**Version:** v1
**Depends on:** TASK-004, TASK-005
**Status:** Not started

## Objective
Persist visible conversation state in a bounded append-only session log so the harness can resume sessions safely after restart without storing hidden reasoning or unsafe payloads.

## Context
Build Order Phase 1, Step 6: "Implement: bounded JSONL records; redaction before write; recovery from partial final line; `/new`; `/resume`." Section 9.1 stores sessions under `$XDG_DATA_HOME/local-agent-harness/sessions/`, never in the target repository. Section 9.2 permits only visible user text, visible assistant text, timestamps, selected model ID, context telemetry, slash-command identity, bounded action/verifier summaries, and task/artifact references. It forbids hidden reasoning fields, raw SSE frames, full provider responses, credentials, credential references, denied-path contents, unbounded tool output, and raw untrusted source payloads by default. Section 9.3 requires recovery to read valid complete JSONL lines and ignore a trailing partial line.

## Scope
- Write bounded append-only JSONL session records in the harness-owned data directory.
- Redact sensitive content before writing.
- Persist only the allowed fields from Section 9.2.
- Recover sessions by reading valid complete lines and ignoring a trailing partial line.
- Implement `/new` and `/resume`.
- Ensure a crash during streaming does not create a false assistant completion.

## Acceptance Criteria
- A restarted application restores visible conversation history.
- Recovery ignores a trailing partial line without corrupting earlier state.
- A crash during streaming preserves valid earlier session state and does not create a false assistant completion.
- Redaction occurs before write.
- Forbidden fields and payloads are not persisted.
- Sessions live outside the target repository.

## Out of Scope
- Slash prompt templates and UI/context commands (TASK-011).
- Wiki or evidence artifact persistence.
- Tool output persistence beyond bounded summaries.
- Raw provider payload storage.
---
