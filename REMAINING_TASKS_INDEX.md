# Local Agent Harness — Remaining Work Index

## Purpose

This index decomposes the active work in `REMAINING_TASKS.md` into small,
reviewable implementation tasks suitable for a local coding model.

The active execution substrate is **fixed reviewed helper scripts** invoked by a
Rust-owned script runner. MCP is retained as frozen prototype/reference work and
is not a dependency for the P0 workflow.

`TASK-000-project-rules.md` applies to every task. Do not start a task until all
listed dependencies are accepted.

## Current State

- Historical accepted baseline: `stability-demo-review`.
- Accepted MCP runtime, containment, provenance, and demonstration work remains
  valuable reference/security foundation.
- Read-only `filesystem_mcp` work is frozen prototype/reference work; it is not
  the next production dependency.
- The active first deliverable is a fixed Rust script runner followed by shared
  path enforcement and read-only helper scripts.
- Existing approval, policy, audit, Wiki, verification, local LLM, TUI, and
  endpoint-profile code should be reused rather than replaced.

## Next Allowed Task

`TASK-010-script-runner-foundation.md`

## Status Legend

- **Blocked** — one or more dependencies are not accepted.
- **Ready** — dependencies are accepted and the task may be assigned.
- **Awaiting review** — implementation is complete but not accepted.
- **Accepted** — reviewed and approved.
- **Frozen reference** — retained historical/prototype work; do not extend in
  the active P0 sequence.

## Active Task Map

| ID | Task | Initial status | Depends on |
| --- | --- | --- | --- |
| TASK-000 | Project rules and delivery contract | Accepted | — |
| TASK-010 | Fixed Rust script-runner foundation | Ready | — |
| TASK-011 | Script protocol, bounded I/O, and error mapping | Blocked | TASK-010 |
| TASK-012 | Script environment, timeout, cancellation, and audit integration | Blocked | TASK-011 |
| TASK-020 | Shared project-root and sensitive-path guard | Blocked | TASK-010 |
| TASK-021 | Existing-path and new-parent symlink-escape enforcement | Blocked | TASK-020 |
| TASK-030 | `read_file` helper script | Blocked | TASK-012, TASK-021 |
| TASK-031 | `git_status` helper script | Blocked | TASK-012, TASK-020 |
| TASK-032 | `git_diff` helper script | Blocked | TASK-012, TASK-020 |
| TASK-033 | Read-only action dispatch and bounded model context | Blocked | TASK-030–032 |
| TASK-040 | Investigate mode and evidence brief | Blocked | TASK-033 |
| TASK-041 | Plan mode and durable plan artifact | Blocked | TASK-040 |
| TASK-050 | Write-file schema and policy preflight | Blocked | TASK-012, TASK-021 |
| TASK-051 | New-file creation with approval and atomic write | Blocked | TASK-050 |
| TASK-052 | Guarded overwrite with expected SHA-256 | Blocked | TASK-051 |
| TASK-060 | Script-backed deterministic verifier | Blocked | TASK-012 |
| TASK-061 | Verification gate integration after mutation | Blocked | TASK-052, TASK-060 |
| TASK-070 | Change-mode end-to-end disposable-repository pilot | Blocked | TASK-040, TASK-041, TASK-061 |
| TASK-071 | Live local-model and TUI pilot | Blocked | TASK-070 |
| TASK-072 | Disposable real-repository acceptance and recovery drills | Blocked | TASK-071 |
| TASK-080 | Operator-approved local checkpoint workflow and runbook | Blocked | TASK-072 |
| TASK-090 | Release build, installation layout, and checksums | Blocked | TASK-070 |
| TASK-091 | CI and release automation | Blocked | TASK-090 |
| TASK-100 | CLI/TUI first-run usability and `--doctor` | Ready | — |
| TASK-110 | Audit permissions, retention, and rotation | Ready | — |
| TASK-120 | Additional fixed verifier profiles | Blocked | TASK-072 |
| TASK-130 | First-class microtask manifests/runtime | Blocked | TASK-080, TASK-072 |

## Frozen Reference Work

| Area | Status | Rule |
| --- | --- | --- |
| MCP runtime/client/process containment | Frozen reference | Do not expand during P0 script-substrate work |
| Filesystem MCP server | Frozen reference | Do not make it a prerequisite for read-only skills |
| Git MCP server | Not active | Do not start without a new architecture decision |
| Shell MCP server | Not active | Do not start without a new architecture decision |
| Dynamic MCP discovery/arbitrary dispatch | Deferred | Requires new product and security decision |

## Explicitly Deferred

Do not start without a new product and security decision:

- dynamic or remote MCP discovery;
- arbitrary MCP dispatch;
- generic shell execution or shell command strings;
- model-selected executables, scripts, or inline code;
- arbitrary Python execution;
- package installation or dependency upgrades;
- generic network access;
- Git push, fetch, reset, checkout, clean, rebase, merge, tags, or submodules;
- automatic commits;
- patch/generalized multi-file transformation support;
- separate work-API adapter while OpenAI-compatible behavior is sufficient;
- multi-user, multi-tenant, or concurrent-task execution;
- autonomous self-modification.

## Execution Order

```text
TASK-010 → TASK-011 → TASK-012
                     ├→ TASK-020 → TASK-021 → TASK-030
                     │                         TASK-031
                     │                         TASK-032
                     │                              ↓
                     │                         TASK-033 → TASK-040 → TASK-041
                     └→ TASK-050 → TASK-051 → TASK-052
                     └→ TASK-060
TASK-052 + TASK-060 → TASK-061 → TASK-070 → TASK-071 → TASK-072 → TASK-080
TASK-070 → TASK-090 → TASK-091
```plaintext

`TASK-100` and `TASK-110` may proceed independently, but their changes must not
change task authority, add execution capabilities, or interfere with P0 work.


