# TASK-071 — Live Local-Model and TUI Pilot

**Initial status:** Blocked  
**Depends on:** TASK-070 accepted

## Goal

Validate actual loopback model streaming and TUI control behavior with the
fixed-script workflow.

## Scope

Use a disposable repository and supported local `llama-server` endpoint. Run
read-only and one approved mutation path; exercise approval denial, Esc,
Ctrl-C, terminal restoration, and child-process cleanup.

## Required Tests / Evidence

Record model/runtime version, smoke-test result, audit excerpts, and operator
observations. Keep this integration test manually invoked/ignored if it requires
a locally running model.

## Acceptance Criteria

- Visible streaming works without persisting hidden reasoning.
- No endpoint fallback or unintended egress occurs.
- Cancellation/quit cannot create false completion or orphan scripts.

## Non-Goals

Do not claim broad model compatibility from one model/template run.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
