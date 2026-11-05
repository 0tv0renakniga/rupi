---
# TASK-016: Implement the read-only Explorer pane

**Version:** v1
**Depends on:** TASK-001, TASK-009, TASK-013, TASK-014, TASK-015
**Status:** Not started

## Objective
Turn the Explorer placeholder into a read-only repository navigator that requests authorized reads through typed UI commands and never bypasses policy or performs filesystem I/O in rendering code.

## Context
Build Order Phase 3, Step 10 begins with the Explorer and native read-only tools. Section 7.3 says the Explorer is read-only initially and may show a bounded project tree, permitted files, selected-file metadata, session-written paths, `.ai-wiki` artifacts, and optional Git state indicators. It must not directly use `std::fs` from rendering code, bypass `Policy`, reveal denied or sensitive paths, write/rename/delete/execute files, or launch an editor based on model output. Explorer selection emits a typed UI command; an application worker performs authorized reads and sends bounded results back.

## Scope
- Replace the Explorer placeholder with a read-only project navigator.
- Show a bounded project tree and permitted files.
- Show selected-file metadata, session-written paths, and `.ai-wiki` artifacts.
- Optionally show Git state indicators when the Git provider is available.
- Emit typed UI commands for selection and authorized read actions.
- Perform reads in an application worker, not in rendering code.
- Hide denied and sensitive paths.

## Acceptance Criteria
- Explorer contents are bounded and policy-compliant.
- Rendering code never directly uses `std::fs`.
- Explorer selections emit typed UI commands.
- Authorized reads are performed by an application worker with bounded results.
- Denied and sensitive paths are not revealed.
- The Explorer cannot write, rename, delete, execute, or launch an editor based on model output.

## Out of Scope
- Native tool implementations (TASK-013, TASK-014, TASK-015).
- Optional Git provider implementation (TASK-017).
- External editor handoff.
- Mutation from the Explorer.
- Unbounded whole-tree scanning.
---
