# TASK-120 — Additional Fixed Verifier Profiles

**Initial status:** Blocked  
**Depends on:** TASK-072 accepted

## Goal

Add only pilot-justified deterministic verifier profiles.

## Scope

Use findings from real-repository pilots to add static argv-only profiles,
prerequisite checks, docs, and tests. Unsupported repos stay blocked.

## Required Tests / Evidence

Profile-specific tests, missing prerequisite test, and documentation update.

## Acceptance Criteria

- Model cannot alter commands.
- Each profile is deterministic, bounded, and documented.
- No dependency installation is introduced.

## Non-Goals

Do not add generic user/model commands.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
