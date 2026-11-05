---
# TASK-034: Add the issue workflow profile

**Version:** v2
**Depends on:** TASK-033
**Status:** Not started

## Objective
Add the reviewed `issue` workflow profile over the existing task state machine without expanding policy or action authority.

## Context
Build Order Phase 4, Step 15 lists "issue" as a reviewed profile to add over the same state machine. Section 11.1 lists workflow commands such as `/issue` as future workflow profiles that start a reviewed workflow profile and remain unavailable until their static profiles exist. Section 20.3 requires workflow profiles not to expand policy authority.
⚠️ Source plan underspecifies this item — verify before starting. The source names the `issue` profile but does not define its detailed lifecycle, inputs, or evidence requirements.

## Scope
- Define the static, reviewed `issue` workflow profile.
- Register it over the existing single-agent state machine.
- Ensure it uses only permitted existing task modes, skills, tools, and evidence mechanisms.
- Keep the `/issue` workflow command gated on the profile's existence.
- Ensure the profile cannot expand policy or task authority.

## Acceptance Criteria
- A static reviewed `issue` profile exists and is registered.
- The profile runs over the existing state machine.
- The `/issue` command is available only when the profile exists.
- The profile cannot expand policy or task authority.
- The profile definition is reviewed and static.
- Underspecified lifecycle, inputs, and evidence requirements are confirmed before completion.

## Out of Scope
- The workflow profile framework (TASK-033).
- Additional workflow profiles (TASK-035 through TASK-037).
- Fuzzing or security-remediation workflow.
- Dynamic or model-authored workflow definitions.
- Expanding policy authority.
---
