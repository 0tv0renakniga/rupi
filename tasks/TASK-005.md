---
# TASK-005: Implement context admission control and per-turn telemetry

**Version:** v1
**Depends on:** TASK-000, TASK-004
**Status:** Not started

## Objective
Prevent oversized model requests before network I/O and expose bounded per-turn operational telemetry so local-model stability can be measured and calibrated.

## Context
Build Order Phase 1, Step 5 includes "context admission control; telemetry display." Section 8.4 requires that before sending a request: `estimated_prompt_tokens + reserved_output_tokens + safety_margin ≤ effective_context_cap_tokens`. If the request does not fit, the application must compact or reject it before network I/O. The effective cap must be conservative and calibrated against observed local-model stability. Section 8.5 lists per-turn metadata including estimated prompt tokens, reserved output tokens, effective context cap, utilization, time to first token, visible output estimate, finish reason, empty visible response flag, and transport error category. The header may show context utilization, TTFT, output estimate, and finish reason.

## Scope
- Estimate prompt tokens and apply reserved output tokens plus a safety margin.
- Enforce the admission formula against the effective context cap before network I/O.
- Compact or reject an oversized request rather than sending it.
- Record bounded per-turn telemetry for the fields listed in Section 8.5.
- Display context utilization, TTFT, output estimate, and finish reason in the Chat header.
- Keep telemetry bounded and free of hidden reasoning or sensitive data.

## Acceptance Criteria
- Oversized requests are blocked or compacted before network I/O.
- The admission formula uses the configured effective context cap, reserved output, and safety margin.
- Per-turn telemetry records the listed operational fields.
- The Chat header displays context utilization, TTFT, output estimate, and finish reason.
- Telemetry excludes hidden reasoning, credentials, and unbounded content.

## Out of Scope
- Loading global or project context files.
- Persisting telemetry to sessions.
- Remote endpoint selection.
- Task-mode or skill context.
---
