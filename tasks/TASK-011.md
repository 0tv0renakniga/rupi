---
# TASK-011: Implement trusted slash prompts and UI/context/session commands

**Version:** v1
**Depends on:** TASK-006, TASK-008, TASK-010
**Status:** Not started

## Objective
Add a bounded slash-command parser and trusted static prompt templates so operators can invoke UI, context, session, and prompt actions without shell or path interpretation.

## Context
Build Order Phase 2, Step 9: "Implement trusted static templates and UI/session/context commands." Section 11.1 defines UI commands such as `/help`, `/tab chat`, `/new`, and `/status`; context commands such as `/context` and `/reload-context`; session commands such as `/resume <id>`; prompt templates such as `/plan`, `/review`, `/explain`, and `/summarize`; and future workflow commands such as `/issue`, `/cve`, and `/fuzz`. Section 11.2 requires templates to load only from the trusted configuration directory, template IDs to be constrained identifiers, repository prompt files not to be automatically trusted, slash commands never to be interpreted as shell commands or arbitrary paths, unknown commands to return a bounded UI error, and workflow commands to remain unavailable until their static profiles exist.

## Scope
- Implement a slash-command parser for the supported command categories.
- Implement UI commands, context commands, and session commands.
- Load prompt templates only from the trusted configuration directory.
- Constrain template IDs to safe identifiers.
- Return bounded UI errors for unknown commands.
- Keep workflow commands unavailable.
- Ensure slash commands are never treated as shell commands or arbitrary paths.

## Acceptance Criteria
- UI, context, session, and prompt-template commands work as specified.
- Templates load only from trusted configuration.
- Repository prompt files are not treated as trusted templates.
- Unknown commands return a bounded UI error.
- Slash input is never interpreted as a shell command or path.
- Workflow commands are unavailable until their static profiles exist.

## Out of Scope
- Workflow command profiles (`/issue`, `/cve`, `/fuzz`) from v2.
- Generic shell execution.
- Arbitrary filesystem paths in commands.
- Model-authored templates.
---
