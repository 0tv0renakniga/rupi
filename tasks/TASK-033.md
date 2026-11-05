---
# TASK-033: Implement the workflow profile framework

**Version:** v2
**Depends on:** TASK-032
**Status:** Not started

## Objective
Add a reviewed workflow-profile layer over the existing state machine so specialized workflows can be registered without expanding policy or task authority.

## Context
Build Order Phase 4, Step 15: "Add reviewed profiles over the same state machine." Section 11.1 lists future workflow commands such as `/issue`, `/cve`, and `/fuzz` and states that workflow commands remain unavailable until their static profiles exist. Section 20.3 requires workflow profiles not to expand policy authority. Step 15 also states that a future fuzzing/security-remediation workflow requires a separate resource and sandboxing design before implementation.
⚠️ Source plan underspecifies this item — verify before starting.

## Scope
- Define the static workflow-profile registration mechanism over the single-agent state machine.
- Bind each profile to reviewed static configuration.
- Ensure profiles cannot expand policy authority, tool authority, or approval requirements.
- Gate workflow slash commands behind the existence of their static profiles.
- Provide a common structure for the initial `issue`, `cve_triage`, `change_review`, and `docs_update` profiles.

## Acceptance Criteria
- Workflow profiles register over the existing state machine.
- Profiles are static and reviewed.
- Profiles cannot expand policy or task authority.
- Workflow slash commands remain unavailable until their profile exists.
- The framework supports the four initial profile names from Step 15.
- No sandbox or fuzzing workflow is introduced.

## Out of Scope
- Implementing the individual profiles (TASK-034 through TASK-037).
- Fuzzing or security-remediation workflow.
- Dynamic profile discovery.
- Model-authored workflows.
- Expanding policy authority.
---
