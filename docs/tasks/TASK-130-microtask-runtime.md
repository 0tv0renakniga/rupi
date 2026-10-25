# TASK-130 — First-Class Microtask Manifests and Runtime

**Initial status:** Blocked  
**Depends on:** TASK-080 and TASK-072 accepted

## Goal

Promote proven operator microtask conventions into constrained runtime data.

## Scope

Define reviewed microtask schema with ID, goal, non-goals, allowed files,
mode, allowed skills, verifier profile, completion rule, and checkpoint policy.
Enforce allowed-file scope as an extra restriction, never as a replacement for
root/path policy.

## Required Tests / Evidence

Schema validation, scope enforcement, audit/Wiki identity, approval linkage,
and checkpoint-policy tests.

## Acceptance Criteria

- Microtask data cannot grant authority.
- Model-generated microtasks require operator review.
- Auto-commit remains disabled by default.

## Non-Goals

Do not implement autonomous planning, concurrent tasks, or self-modifying
manifests.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
