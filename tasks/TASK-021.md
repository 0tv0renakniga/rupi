---
# TASK-021: Implement Investigate mode

**Version:** v1
**Depends on:** TASK-018, TASK-019, TASK-020
**Status:** Not started

## Objective
Add the read-only Investigate mode so the harness can follow the Learn → Review → Compile lifecycle and complete only when a durable evidence brief exists.

## Context
Build Order Phase 3, Step 12: "Implement durable evidence, plan, and review artifacts." Section 15.1 defines Investigate as `Learn → Review → Compile` with mutation forbidden. Section 13.4 lists Investigate completion evidence as an "Evidence brief." Section 5.2 and Phase 3 exit criteria require the model to inspect a repository and produce useful bounded artifacts without mutation.

## Scope
- Implement Investigate mode over the read-only tool and skill substrate.
- Restrict the lifecycle to Learn → Review → Compile.
- Forbid mutation in Investigate mode.
- Produce a bounded, durable evidence brief using the artifact writer and compilation pipeline.
- Gate completion on the existence of the required evidence artifact.

## Acceptance Criteria
- Investigate mode follows Learn → Review → Compile.
- Mutation actions are unavailable in Investigate mode.
- Completion requires a durable evidence brief.
- The evidence brief is bounded, redacted, and attributable.
- Repository inspection remains within the selected skill and policy authority.
- The mode cannot complete based only on a model claim.

## Out of Scope
- Plan and Review modes (TASK-022, TASK-023).
- Mutation, approval, and verification.
- Workflow profiles.
- Unbounded repository inspection.
---
