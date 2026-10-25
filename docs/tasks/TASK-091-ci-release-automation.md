# TASK-091 — CI and Release Automation

**Initial status:** Blocked  
**Depends on:** TASK-090 accepted

## Goal

Continuously validate the active script-substrate path and package release
artifacts on approved version tags.

## Scope

Add script protocol/helper-script test coverage to existing CI, preserve
remote-feature/hardened-profile coverage, and add tag-triggered release package
and SHA256SUMS artifact generation. Keep live-model tests out of normal CI.

## Required Tests / Evidence

Verify CI configuration locally where possible and inspect first actual CI
runs. Ensure config-template/documentation checks cover runtime variable names.

## Acceptance Criteria

- Default and remote feature lanes pass.
- Hardened lane remains required when configured.
- Version tag emits checksummed artifact without live model/network dependency.

## Non-Goals

Do not auto-publish releases without an explicit product decision.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
