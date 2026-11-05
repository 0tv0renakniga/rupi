---
# TASK-001: Build the static Ratatui shell

**Version:** v1
**Depends on:** TASK-000
**Status:** Not started

## Objective
Create the static TUI shell that establishes layout, navigation, focus, and fixture-driven rendering before any live model, filesystem, or tool I/O is connected.

## Context
Build Order Phase 1, Step 2: "Implement: header; tabs; persistent Explorer placeholder; prompt input; Output panel; help overlay; focus model; responsive layout tests." The TUI architecture in Section 7.1 describes a persistent Explorer side pane and tabs for Chat, Context, Diff, Output, and Session. Section 7.2 defines the initial roles of those tabs. The TUI must display and control only; it must not perform direct model, filesystem, or tool I/O.

## Scope
- Implement the header with project, model, context, session, endpoint, mode, and tool indicators.
- Implement the Chat, Context, Diff, Output, and Session tabs.
- Add a persistent Explorer placeholder pane.
- Add prompt input, the Output panel, and the help overlay.
- Implement a focus model for tabs, Explorer, and prompt input.
- Add responsive layout tests using Ratatui `TestBackend`.
- Render all initial tab content from static fixtures.

## Acceptance Criteria
- All tabs render from fixtures.
- The Explorer placeholder renders as a persistent side pane.
- Focus can move between the main areas through typed UI commands.
- Responsive layout tests cover resize behavior.
- No network, tool, or filesystem I/O is reachable from drawing code.

## Out of Scope
- Live chat streaming or model requests.
- Transcript rendering from real SSE events.
- Real Explorer contents or filesystem reads.
- External editor handoff.
- Session persistence.
---
