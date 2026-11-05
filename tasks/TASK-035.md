---
# TASK-035: Add the cve_triage workflow profile

**Version:** v2
**Depends on:** TASK-033
**Status:** Not started

## Objective
Add the reviewed `cve_triage` workflow profile over the existing task state machine without expanding policy or action authority.

## Context
Build Order Phase 4, Step 15 lists "cve_triage" as a reviewed profile to add over the same state machine. Section 11.1 lists workflow commands such as `/cve` as future workflow profiles that start a reviewed workflow profile and remain unavailable until their static profiles exist. Section 20.3 requires workflow profiles not to expand policy authority.
⚠️ Source plan underspecifies this item — verify before starting. The source names the `cve_triage` profile but does not define its detailed lifecycle, inputs, or evidence requirements.

## Scope
- Define the static, reviewed `cve_triage` workflow profile.
- Register it over the existing single-agent state machine.
- Ensure it uses only permitted existing task modes, skills, tools, and evidence mechanisms.
- Keep the `/cve` workflow command gated on the profile's existence.
- Ensure the profile cannot expand policy or task authority.

## Acceptance Criteria
- A static reviewed `cve_triage` profile exists and is registered.
- The profile runs over the existing state machine.
- The `/cve` command is available only when the profile exists.
- The profile cannot expand policy or task authority.
- The profile definition is reviewed and static.
- Underspecified lifecycle, inputs, and evidence requirements are confirmed before completion.

## Out of Scope
- The workflow profile framework (TASK-033).
- Additional workflow profiles (TASK-034, TASK-036, TASK-037).
- Fuzzing or security-remediation workflow.
- Network vulnerability feeds or automatic package updates.
- Dynamic or model-authored workflow definitions.
---
