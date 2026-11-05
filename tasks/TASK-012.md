---
# TASK-012: Assemble bounded context projections for model requests

**Version:** v1
**Depends on:** TASK-006, TASK-008, TASK-010, TASK-011
**Status:** Not started

## Objective
Assemble each model request from bounded context projections rather than replaying the full transcript, raw repository dumps, or complete tool history.

## Context
Section 10.3 states that context is assembled from: system rules; trusted global context; selected untrusted project context; session summary; selected prompt template; task/skill state when applicable; bounded recent action outcomes; and verification evidence when applicable. It explicitly states: "The full transcript, raw repository dumps, and complete tool history are not replayed every turn." Section 8.4 requires the assembled request to pass admission control before network I/O.

## Scope
- Implement a context projection assembler for the sources listed in Section 10.3.
- Include system rules, trusted global context, selected untrusted project context, session summary, selected prompt template, applicable task/skill state, bounded recent action outcomes, and applicable verification evidence.
- Keep projections bounded and selectively included.
- Integrate the assembled projection with context admission control.
- Avoid replaying the full transcript, raw repository dumps, or complete tool history.

## Acceptance Criteria
- Context is assembled from the listed bounded projections.
- Only selected context is included for each request.
- The full transcript, raw repository dumps, and complete tool history are not replayed every turn.
- Project context remains wrapped and untrusted.
- Assembled requests pass the admission-control check before network I/O.
- Projection sizes are bounded by configuration.

## Out of Scope
- Loading the underlying global or project context sources (TASK-008, TASK-010).
- Slash command parsing (TASK-011).
- Token estimation and telemetry mechanics (TASK-005).
- Task-loop or skill state implementation (v2).
---
