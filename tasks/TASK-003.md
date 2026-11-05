---
# TASK-003: Implement SSE stream normalization and hidden-reasoning policy

**Version:** v1
**Depends on:** TASK-000
**Status:** Done

## Objective
Isolate provider and model quirks in a compatibility layer that converts raw SSE bytes into normalized visible chat events while excluding hidden reasoning from all user-visible and persisted paths.

## Context
Build Order Phase 1, Step 4: "raw SSE → normalized visible events → finish/error classification." Tests must cover split UTF-8 chunks, split SSE lines, `[DONE]`, malformed frames, reasoning-only frames, empty visible completion, and early disconnect. Section 8.2 defines `VisibleChatEvent`. Section 8.3 requires fields such as `reasoning_content`, `reasoning`, `thinking`, and `analysis` to be ignored for visible rendering and excluded from session persistence, audit records, Wiki compilation, and future model context. The TUI must never parse provider-specific SSE fields.

## Scope
- Implement the raw-SSE-to-`VisibleChatEvent` parser and finish/error classification.
- Handle split UTF-8 chunks, split SSE lines, `[DONE]`, malformed frames, reasoning-only frames, empty visible completions, and early disconnects.
- Classify empty visible completions as an empty-response warning, not assistant success.
- Ignore hidden reasoning fields for visible transcript rendering.
- Exclude hidden reasoning fields from session, audit, Wiki, and future model context paths.
- Optionally record only a bounded operational classification such as `hidden_reasoning_field_observed = true`.

## Acceptance Criteria
- Provider quirks are isolated outside the TUI.
- Split UTF-8 and split SSE lines normalize correctly.
- `[DONE]`, malformed frames, reasoning-only frames, empty completions, and early disconnects are classified deterministically.
- Hidden reasoning is never emitted as visible text.
- Hidden reasoning fields are excluded from persistence, audit, Wiki, and context paths.
- The TUI receives only normalized visible events.

## Out of Scope
- TUI rendering and transcript layout.
- HTTP request construction, retries, or active-turn cancellation.
- Session file writing.
- Tool-call event support.
---
