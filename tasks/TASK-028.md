---
# TASK-028: Implement the optional trusted script substrate

**Version:** v2
**Depends on:** TASK-009, TASK-025
**Status:** Not started

## Objective
Provide an optional trusted-script capability with manifest pinning and typed actions so reviewed external tools can run without exposing a general model-controlled script or argv interface.

## Context
Section 12.4 describes scripts as optional, trusted operator-owned capabilities located outside the project root. A script must have a canonical trusted path, manifest entry, SHA-256 pin, bounded timeout, script-specific typed input, minimal environment, bounded stdout/stderr, and audit provenance. The model must not supply arbitrary `Vec<String>` argv values to a general `run_script` capability. Instead, Rust exposes reviewed typed script actions such as `ScriptAction::RunVerifier { profile_id }` and `ScriptAction::RunFormatter { profile_id }`; Rust validates typed fields and constructs argv itself.

## Scope
- Define the trusted-script manifest and canonical trusted-path requirements.
- Enforce SHA-256 pin validation before execution.
- Enforce bounded timeouts and minimal environment.
- Define typed script actions with script-specific input.
- Construct argv in Rust from validated typed fields.
- Bound stdout and stderr.
- Record audit provenance.
- Keep the capability optional and outside the project root.

## Acceptance Criteria
- Trusted scripts are located outside the project root and have canonical trusted paths.
- Every script requires a manifest entry and matching SHA-256 pin.
- Timeouts, environment, and stdout/stderr are bounded.
- The model cannot supply arbitrary argv values.
- Rust constructs argv from validated typed action fields.
- Audit provenance is recorded.
- The harness works without this optional capability unless a selected action requires it.

## Out of Scope
- A general `run_script` capability.
- Model-selected executables, scripts, interpreters, or working directories.
- Arbitrary Python execution.
- Package installation or dependency updates.
- Mutating script capabilities beyond reviewed typed actions.
---
