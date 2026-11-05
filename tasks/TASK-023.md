---
# TASK-023: Implement Review mode

**Version:** v1
**Depends on:** TASK-018, TASK-019, TASK-020
**Status:** Not started

## Objective
Add the read-only Review mode so the harness can follow the Learn → Review → Compile lifecycle and complete only when durable review findings exist.

## Context
Build Order Phase 3, Step 12: "Implement durable evidence, plan, and review artifacts." Section 15.1 defines Review as `Learn → Review → Compile` with mutation forbidden. Section 13.4 lists Review completion evidence as "Durable review findings." Section 15.4 describes review as one bounded model turn after required evidence is present; in Change mode it is part of the completion path.

## Scope
- Implement Review mode over the read-only tool and skill substrate.
- Restrict the lifecycle to Learn → Review → Compile.
- Forbid mutation in Review mode.
- Produce bounded, durable review findings using the artifact writer and compilation pipeline.
- Gate completion on the existence of the required review artifact.
- Keep review to a bounded model turn.

## Acceptance Criteria
- Review mode follows Learn → Review → Compile.
- Mutation actions are unavailable in Review mode.
- Completion requires durable review findings.
- Review findings are bounded, redacted, and attributable.
- Review is one bounded model turn after required evidence is present.
- The mode cannot complete based only on a model claim.

## Out of Scope
- Investigate and Plan modes (TASK-021, TASK-022).
- Change-mode review and completion gating (v2).
- Mutation, approval, and verification.
- Workflow profiles.
---
