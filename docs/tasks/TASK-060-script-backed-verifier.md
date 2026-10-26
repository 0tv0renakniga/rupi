# TASK-060 — Script-Backed Deterministic Verifier

**Initial status:** Blocked  
**Status:** Accepted
**Depends on:** TASK-012 accepted

## Goal

Run harness-selected fixed verification profiles through the script boundary
without adding generic command authority.

## Scope

Implement `scripts/run_verifier.py` and runner integration. Accept only
harness-generated argv verification commands. Preserve existing Rust/Node/Python
profile detection, bounded output, duration, timeout, exit status, and missing-
dependency blocking behavior. Do not auto-install dependencies.

## Required Tests / Evidence

Tests for argv preservation, no shell, passing/failing command, timeout,
output cap, unsupported project, and missing dependency blocker.

## Acceptance Criteria

- Model cannot provide verifier program/args.
- Each verifier step is argv-only and bounded.
- Unsupported/missing prerequisites block rather than trigger installation.

## Non-Goals

Do not add package installation, custom model commands, or network access.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
