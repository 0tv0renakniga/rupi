---
# TASK-031: Implement stale-verification and external-change detection

**Version:** v2
**Depends on:** TASK-029, TASK-030
**Status:** Not started

## Objective
Detect relevant file changes after verification so prior verification cannot be reused and a Change task cannot complete on stale evidence.

## Context
Build Order Phase 4, Step 14 lists "stale-verification detection." Section 16.3 states that the harness always records paths it writes and their before/after hashes. For relevant Change tasks it may compare harness-written path hashes, explicitly selected external-editor file hashes, and configured project manifest hashes to detect changes after verification. A bounded broader repository scan may later be added, but whole-tree hashing is not required in the first mutation release.

## Scope
- Record before/after hashes for every path written by the harness.
- Compare harness-written path hashes after verification.
- Compare hashes for explicitly selected external-editor files.
- Compare configured project manifest hashes.
- Invalidate prior verification when relevant content changes.
- Keep the detection bounded and relevant to the Change task.

## Acceptance Criteria
- Harness-written paths and their before/after hashes are recorded.
- Relevant changes after verification invalidate prior verification.
- Explicitly selected external-editor files are included when configured.
- Configured project manifest hashes are included.
- Whole-tree hashing is not required for the first release.
- A Change task cannot complete using stale verification evidence.

## Out of Scope
- Whole-repository hashing.
- External editor handoff implementation.
- Automatic Git checkpoints or resets.
- Mutation execution itself (TASK-029).
- Verifier profile execution (TASK-030).
---
