---
# TASK-004: Implement functional Chat turn execution

**Version:** v1
**Depends on:** TASK-001, TASK-002, TASK-003
**Status:** Not started

## Objective
Connect the Chat tab to the normalized local-model stream so a user can send a prompt, watch visible output stream, cancel safely, and see clear empty-response or endpoint-failure states.

## Context
Build Order Phase 1, Step 5: "Implement: send prompt; stream visible output; cancel active turn; empty response warning; endpoint failure display; context admission control; telemetry display." The exit criteria are: "A local llama-server can complete a simple chat request. Esc cancels safely. No hidden reasoning is rendered or persisted." Section 8.6 states that an empty visible completion is not assistant success: it produces an `EmptyResponse` warning, adds no empty assistant message to history, and allows the operator to use `/retry`.

## Scope
- Send a prompt from the Chat tab to the local OpenAI-compatible endpoint.
- Stream normalized visible events into the transcript.
- Support cancellation of an active turn with Esc.
- Display empty-response warnings and endpoint failures.
- Ensure empty visible completions do not create empty assistant history entries.
- Keep hidden reasoning out of rendered transcript state.

## Acceptance Criteria
- A local `llama-server` can complete a simple chat request.
- Visible output streams incrementally into the Chat tab.
- Esc cancels the active turn safely.
- Empty responses produce a warning and no empty assistant message.
- Endpoint failures are displayed to the operator.
- No hidden reasoning is rendered.

## Out of Scope
- Context admission control and telemetry display (TASK-005).
- Session persistence and resume (TASK-006).
- Global or project context loading.
- Slash commands and prompt templates.
- Task loops, tools, or mutation.
---
