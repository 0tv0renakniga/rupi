---
# TASK-037: Add the docs_update workflow profile

**Version:** v2
**Depends on:** TASK-033
**Status:** Not started

## Objective
Add the reviewed `docs_update` workflow profile over the existing task state machine without expanding policy or action authority.

## Context
Build Order Phase 4, Step 15 lists "docs_update" as a reviewed profile to add over the same state machine. Section 20.3 requires workflow profiles not to expand policy authority.
⚠️ Source plan underspecifies this item — verify before starting. The source names the `docs_update` profile but does not define its detailed lifecycle, inputs, or evidence requirements.

## Scope
- Define the static, reviewed `docs_update` workflow profile.
- Register it over the existing single-agent state machine.
- Ensure it uses only permitted existing task modes, skills, tools, and evidence mechanisms.
- Ensure any mutation path remains subject to the existing approval and verification requirements.
- Ensure the profile cannot expand policy or task authority.

## Acceptance Criteria
- A static reviewed `docs_update` profile exists and is registered.
- The profile runs over the existing state machine.
- The profile cannot expand policy or task authority.
- Any mutation remains subject to approval and verification.
- The profile definition is reviewed and static.
- Underspecified lifecycle, inputs, and evidence requirements are confirmed before completion.

## Out of Scope
- The workflow profile framework (TASK-033).
- Additional workflow profiles (TASK-034 through TASK-036).
- Fuzzing or security-remediation workflow.
- Bypassing approval or verification for documentation edits.
- Dynamic or model-authored workflow definitions.
---
