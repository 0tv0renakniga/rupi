# TASK-100 — CLI/TUI First-Run Usability and Doctor Mode

**Initial status:** Ready  
**Depends on:** —

## Goal

Improve operator setup feedback without changing task authority.

## Scope

Add `--help`, `--version`, strict argument errors, startup summary, and
`--doctor` validation for root, audit path, scripts, endpoint profile, and
optional model reachability.

## Required Tests / Evidence

Argument parsing and doctor tests; ensure help/version do not start workers or
helper scripts; verify secret-safe output.

## Acceptance Criteria

- Operators can identify active root, endpoint profile, audit path, and
  available skills.
- Blocking setup errors are actionable.
- No credentials are printed.

## Non-Goals

Do not add a new execution capability or make network reachability mandatory.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
