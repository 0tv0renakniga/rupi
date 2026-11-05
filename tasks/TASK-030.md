---
# TASK-030: Implement verifier profiles and run_verifier

**Version:** v2
**Depends on:** TASK-009, TASK-026, TASK-028
**Status:** Not started

## Objective
Make verification harness-owned and deterministic by executing fixed argv-only verifier profiles with timeouts and blocking completion when dependencies are absent or checks fail.

## Context
Build Order Phase 4, Step 14 lists `run_verifier`. Section 13.1 states that the harness owns verification selection and execution; the model cannot author arbitrary verifier commands, disable required checks, replace a verifier profile, call package managers, or claim a verifier passed without evidence. Section 13.2 places verifier profiles in trusted `verifiers.toml` with fixed argv arrays and timeout settings. Section 13.3 states that dependencies already installed allow a fixed verifier profile to run, while absent or unsupported dependencies must Block with actionable operator instructions. The harness must not automatically run package installers or use `sudo`.

## Scope
- Load verifier profiles from trusted `verifiers.toml`.
- Execute fixed argv arrays with configured timeouts.
- Invoke executables directly without a shell.
- Block with actionable operator instructions when dependencies are absent or unsupported.
- Do not run package managers, installers, or `sudo`.
- Bound verifier output.
- Record verification results as evidence.
- Keep verification selection and execution harness-owned.

## Acceptance Criteria
- Verifier profiles come only from trusted operator configuration.
- Verifier steps use fixed argv arrays and configured timeouts.
- The model cannot author, replace, or disable verifier commands.
- Missing or unsupported dependencies produce a Block with operator instructions.
- No package installation or `sudo` occurs.
- Verifier output is bounded.
- Verification results are recorded as evidence.
- Failed verification blocks completion.

## Out of Scope
- `write_file` mutation (TASK-029).
- Approval-gate behavior (TASK-027).
- External-change detection (TASK-031).
- Automatic dependency installation.
- Model-authored verifier commands.
---
