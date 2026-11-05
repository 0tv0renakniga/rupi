---
# TASK-018: Implement read-only skills

**Version:** v1
**Depends on:** TASK-013, TASK-014, TASK-015
**Status:** Not started

## Objective
Add reviewed static skills that constrain a workflow to a fixed subset of read-only tools and provide repeatable procedures for repository orientation and change review.

## Context
Build Order Phase 3, Step 11: "Implement static skills such as: `repo-orient`, `change-review`." Section 14 defines a skill as "a reviewed, static procedure with a fixed subset of allowed tools." Skills are not model-authored, dynamically discovered, loaded from arbitrary repository paths, or allowed to expand task authority. The example `repo-orient` uses `list_directory`, `read_file`, and `search_text`; `change-review` uses `read_file` and `search_text`. Tool authority is the intersection of task-mode allowlist, selected skill allowlist, global policy, and currently implemented capabilities.

## Scope
- Implement the reviewed static `repo-orient` skill with the documented purpose, tools, and procedure.
- Implement the reviewed static `change-review` skill with the documented purpose, tools, and procedure.
- Enforce the tool-authority intersection: task mode, skill, global policy, and implemented capabilities.
- Ensure skills cannot expand task authority.
- Keep skills static and operator-reviewed.

## Acceptance Criteria
- `repo-orient` and `change-review` exist as static reviewed skills.
- Each skill exposes only its documented tool subset.
- Tool authority is the intersection of mode allowlist, skill allowlist, global policy, and implemented capabilities.
- Skills cannot be model-authored, dynamically discovered, or loaded from arbitrary repository paths.
- Skills cannot expand task authority.

## Out of Scope
- Model-authored or dynamically discovered skills.
- Workflow profiles from v2.
- Mutation-capable skills.
- Task-loop orchestration.
- Loading skills from repository files.
---
