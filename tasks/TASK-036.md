---
# TASK-036: Add the change_review workflow profile

**Version:** v2
**Depends on:** TASK-033
**Status:** Not started

## Objective
Add the reviewed `change_review` workflow profile over the existing task state machine without expanding policy or action authority.

## Context
Build Order Phase 4, Step 15 lists "change_review" as a reviewed profile to add over the same state machine. Section 14 already defines a `change-review` read-only skill for reviewing paths written during a session; this Step 15 profile is a workflow over the same state machine. Section 20.3 requires workflow profiles not to expand policy authority.
⚠️ Source plan underspecifies this item — verify before starting. The source names the `change_review` profile but does not define its detailed lifecycle, inputs, or evidence requirements.

## Scope
- Define the static, reviewed `change_review` workflow profile.
- Register it over the existing single-agent state machine.
- Ensure it uses only permitted existing task modes, skills, tools, and evidence mechanisms.
- Keep the profile distinct from the `change-review` read-only skill while reusing reviewed capabilities where appropriate.
- Ensure the profile cannot expand policy or task authority.

## Acceptance Criteria
- A static reviewed `change_review` profile exists and is registered.
- The profile runs over the existing state machine.
- The profile cannot expand policy or task authority.
- The profile definition is reviewed and static.
- The relationship between the profile and the `change-review` skill is documented and does not broaden authority.
- Underspecified lifecycle, inputs, and evidence requirements are confirmed before completion.

## Out of Scope
- The workflow profile framework (TASK-033).
- Additional workflow profiles (TASK-034, TASK-035, TASK-037).
- The read-only `change-review` skill implementation (TASK-018).
- Fuzzing or security-remediation workflow.
- Expanding policy authority.
---
