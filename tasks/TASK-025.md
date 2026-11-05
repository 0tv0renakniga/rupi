---
# TASK-025: Implement the core single-agent state machine

**Version:** v2
**Depends on:** TASK-018, TASK-021, TASK-022, TASK-023
**Status:** Not started

## Objective
Introduce the central task state machine and mode-specific transition rules so the harness can move from read-only workflows into a single-agent lifecycle without permitting invalid transitions.

## Context
Build Order Phase 4, Step 13: "Implement: `Learn → Plan → Act → Verify → Review → Compile`" with task-mode action filtering and no classifier dependency. Section 15.2 defines the core state machine: Boot → Learn → Plan → Act → Verify → Review → Compile → Completed, with any active state able to transition to Blocked, Cancelled, or Failed. Section 15.1 states that task modes restrict valid transitions and defines Chat, Investigate, Plan, Review, and Change lifecycles.

## Scope
- Implement the core lifecycle states Boot, Learn, Plan, Act, Verify, Review, Compile, and Completed.
- Implement Blocked, Cancelled, and Failed as terminal or exit states from active states.
- Enforce mode-specific transition restrictions for Chat, Investigate, Plan, Review, and Change.
- Provide the central orchestrator state used by later task-loop features.
- Keep the state machine independent of Jev or any external classifier.

## Acceptance Criteria
- The core states and transitions match Section 15.2.
- Each task mode permits only its documented lifecycle transitions.
- Invalid transitions are rejected.
- Any active state can transition to Blocked, Cancelled, or Failed as specified.
- No external routing or classifier service is required.
- Read-only v1 modes continue to operate through the state machine.

## Out of Scope
- Action filtering, budgets, and non-progress policy (TASK-026).
- Mutation, approval, and verification implementation.
- Workflow profiles.
- Concurrent or multi-agent execution.
---
