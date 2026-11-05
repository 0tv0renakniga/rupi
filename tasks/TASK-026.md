---
# TASK-026: Implement task budgets, action filtering, and non-progress policy

**Version:** v2
**Depends on:** TASK-025
**Status:** Not started

## Objective
Constrain task execution with budgets and deterministic non-progress handling so the harness cannot loop indefinitely, exceed configured turn limits, or expose actions outside the active mode and skill.

## Context
Build Order Phase 4, Step 13 requires task budgets, deterministic non-progress behavior, task-mode action filtering, no classifier dependency, and bounded corrective retries. Section 15.3 defines the no-classifier completion policy: no action call plus exact `VERIFIED_DONE` requests verification or continues according to task state; no action call plus exact `NEED_INPUT: ...` becomes Blocked awaiting operator input; no action call plus anything else is a non-progress turn that gets one corrective prompt and then becomes Blocked if non-progress repeats. The harness must not repeatedly nudge indefinitely until consuming the entire turn budget. Section 6.1 provides `max_act_turns` and `max_verify_retries`.

## Scope
- Enforce configured act-turn and verify-retry budgets.
- Apply task-mode and selected-skill action filtering at dispatch time.
- Implement deterministic handling for `VERIFIED_DONE`, `NEED_INPUT: ...`, and other no-action responses.
- Allow one corrective prompt for a non-progress turn, then block if non-progress repeats.
- Prevent indefinite nudging or turn-budget exhaustion.
- Keep completion decisions deterministic and classifier-free.

## Acceptance Criteria
- Task budgets are enforced from configuration.
- Actions outside the active mode or skill authority are rejected.
- No-action `VERIFIED_DONE` requests verification or continues according to task state.
- No-action `NEED_INPUT: ...` blocks awaiting operator input.
- Other no-action turns receive at most one corrective prompt before blocking on repeat.
- The harness does not nudge indefinitely or consume the entire turn budget through repetition.
- No external classifier is used.

## Out of Scope
- Mutation, approval, or verifier implementation.
- Workflow profiles.
- Model-based completion classification.
- Concurrent task execution.
---
