---
# TASK-007: Add the local-model compatibility smoke suite

**Version:** v1
**Depends on:** TASK-004, TASK-005, TASK-006
**Status:** Not started

## Objective
Provide a manually run local-model smoke suite that records compatibility and stability observations without making a live model a requirement for ordinary automated tests.

## Context
Section 19.2 requires maintaining "a manually run local-model smoke suite" that records: model ID; `llama.cpp` / `llama-server` version; context cap; reserved output; time to first token; empty-response rate; finish reasons; cancellation result. It also states: "Do not require a live model in ordinary automated test runs." Section 3.1 identifies local-model reliability, streaming, context saturation, cancellation, and `llama-server` instability as risks to debug before the task loop.

## Scope
- Define a repeatable manual smoke-suite procedure for a local OpenAI-compatible model endpoint.
- Record the listed model, server, context, timing, empty-response, finish-reason, and cancellation fields.
- Keep the suite outside ordinary automated test requirements.
- Capture bounded results suitable for compatibility comparison across model or server changes.

## Acceptance Criteria
- The smoke suite can be run manually against a local `llama-server`.
- It records every field listed in Section 19.2.
- Ordinary automated test runs do not require a live model.
- Results distinguish normal completion, empty responses, finish reasons, and cancellation outcomes.

## Out of Scope
- Automated live-model CI.
- Benchmarking model quality.
- Remote endpoint testing.
- Modifying model or server configuration.
---
