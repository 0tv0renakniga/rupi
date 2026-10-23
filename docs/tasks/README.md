# Local Agent Harness — Remaining Work Index

## Purpose

This directory splits the remaining work into small, reviewable tasks suitable
for local coding models. `TASK-000-project-rules.md` applies to every task.

## Current state

- Accepted baseline: `stability-demo-review`.
- Read-only production `filesystem_mcp` was introduced in `062ed2a` and
  hardened in `f4999c8`.
- It is implemented but requires final full verification and explicit acceptance
  before mutation capability work begins.
- No release-built/installable MCP package exists yet.
- `git_mcp`, mutation-capable filesystem tools, and `shell_mcp` are not yet
  accepted production capabilities.

## Next allowed task

`TASK-010-fs-readonly-accept.md`

Do not start a later task until its dependencies are accepted.

## Status legend

- **Blocked** — dependency not accepted.
- **Ready** — dependencies accepted; may be assigned.
- **Awaiting review** — implemented but not accepted.
- **Accepted** — reviewed and approved.

## Task map

| ID | Task | Initial status | Depends on |
| --- | --- | --- | --- |
| TASK-010 | Finalize read-only filesystem MCP | Awaiting review | — |
| TASK-020 | Write-file schema only | Blocked | TASK-010 |
| TASK-021 | Create new file only | Blocked | TASK-020 |
| TASK-022 | Guarded overwrite | Blocked | TASK-021 |
| TASK-030 | Select patch strategy | Blocked | TASK-022 |
| TASK-031 | Patch schema/rejection paths | Blocked | TASK-030 |
| TASK-032 | Apply bounded single-file patch | Blocked | TASK-031 |
| TASK-040 | Git MCP status | Blocked | TASK-010 |
| TASK-041 | Git MCP diff | Blocked | TASK-040 |
| TASK-050 | Shell schema/validation only | Blocked | TASK-010 |
| TASK-051 | One harmless argv command | Blocked | TASK-050 |
| TASK-052 | Verifier-selected command execution | Blocked | TASK-051 |
| TASK-060–063 | Local-model/TUI smoke and pilots | Blocked | Real required MCP capabilities |
| TASK-070–074 | Disposable real-repository pilot/drills | Blocked | TASK-060–063 |
| TASK-080–083 | Release build/install validation | Blocked | Production MCP set |
| TASK-090–092 | CI/release automation | Blocked | Release build |
| TASK-100–102 | CLI usability | Ready | — |
| TASK-110–111 | Audit permissions/rotation design | Ready | — |
| TASK-120–121 | Verifier blocked/configuration | Ready | — |
| TASK-200–204 | Future task modes | Blocked | Real-server pilots |

## Explicitly deferred

Dynamic MCP discovery, arbitrary MCP tools, remote MCP, Git mutation,
package installation/dependency updates, generic network access, shell command
strings, multi-tenant execution, and a separate work-API adapter remain deferred
until a new product and security decision.
