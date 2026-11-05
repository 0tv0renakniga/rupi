---
# TASK-002: Implement fixture-driven transcript rendering

**Version:** v1
**Depends on:** TASK-001
**Status:** Done

## Objective
Make transcript formatting reproducible and testable with fixtures so Markdown, Unicode, streaming chunks, errors, and truncation can be debugged without a live model or task loop.

## Context
Build Order Phase 1, Step 3 requires fixture-driven transcript state and rendering tests for: normal text; multiline Markdown; code blocks; Unicode; blank lines; long unbroken lines; streaming chunks; error/status message; empty response; truncation indicator. The exit criterion is: "Formatting bugs can be reproduced through TestBackend fixtures." Section 8.1 shows the renderer consuming normalized visible stream events rather than provider-specific SSE.

## Scope
- Implement fixture-driven transcript state and rendering.
- Add TestBackend tests for normal text, multiline Markdown, code blocks, Unicode, blank lines, long unbroken lines, streaming chunks, error/status messages, empty responses, and truncation indicators.
- Ensure the renderer consumes normalized visible events rather than raw provider frames.
- Add stable behavior for wrapping and truncation in long or unbroken content.

## Acceptance Criteria
- Formatting bugs can be reproduced through `TestBackend` fixtures.
- Each listed content case has a fixture and rendering test.
- Streaming chunk fixtures render incrementally without corrupting previous text.
- Empty responses and error/status messages have distinct render paths.
- The renderer does not parse provider-specific SSE fields.

## Out of Scope
- Raw SSE parsing and normalization.
- Network requests or cancellation.
- Session persistence.
- Live model integration.
---
