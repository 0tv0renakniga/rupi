---
# TASK-022: Implement Plan mode

**Version:** v1
**Depends on:** TASK-018, TASK-019, TASK-020
**Status:** Not started

## Objective
Add the read-only Plan mode so the harness can follow the Learn → Plan → Review → Compile lifecycle and complete only when a durable plan artifact exists.

## Context
Build Order Phase 3, Step 12: "Implement durable evidence, plan, and review artifacts." Section 15.1 defines Plan as `Learn → Plan → Review → Compile` with mutation forbidden. Section 13.4 lists Plan completion evidence as a "Durable plan artifact." Phase 3 exit criteria require useful bounded artifacts without mutation.

## Scope
- Implement Plan mode over the read-only tool and skill substrate.
- Restrict the lifecycle to Learn → Plan → Review → Compile.
- Forbid mutation in Plan mode.
- Produce a bounded, durable plan artifact using the artifact writer and compilation pipeline.
- Gate completion on the existence of the required plan artifact.

## Acceptance Criteria
- Plan mode follows Learn → Plan → Review → Compile.
- Mutation actions are unavailable in Plan mode.
- Completion requires a durable plan artifact.
- The plan artifact is bounded, redacted, and attributable.
- Repository inspection remains within the selected skill and policy authority.
- The mode cannot complete based only on a model claim.

## Out of Scope
- Investigate and Review modes (TASK-021, TASK-023).
- Mutation, approval, and verification.
- Workflow profiles.
- Executing the plan.
---
