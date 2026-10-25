# TASK-090 — Release Build, Installation Layout, and Checksums

**Initial status:** Blocked  
**Depends on:** TASK-070 accepted

## Goal

Create an installable local artifact for the script-substrate workflow.

## Scope

Package release binary, reviewed helper scripts, docs, config template, and
SHA256SUMS. Define installed script location, launcher behavior, provenance,
and clean-machine headless validation.

## Required Tests / Evidence

Build with `--release --locked`; validate installed artifact against a
disposable repo without source checkout/debug paths; verify binary/script
checksums.

## Acceptance Criteria

- Installed runner locates only installed reviewed scripts.
- No test mock or source checkout is required.
- Provenance records source commit, Rust version, target triple, and digests.

## Non-Goals

Do not publish automatically or add cloud distribution.

## Delivery

Follow `TASK-000-project-rules.md`. Include a focused diff, tests run and their
results, known limitations, and any configuration/documentation changes.
