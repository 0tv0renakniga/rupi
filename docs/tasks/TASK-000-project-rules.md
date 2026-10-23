# TASK-000 — Project Rules for Small Coding Agents

## Purpose

This file applies to every task in this directory. Read it before editing.

## Core architecture

One orchestrator owns the task lifecycle:

```text
Learn → Plan → Act → Verify → Review → Compile → Complete
```

The model proposes. Policy authorizes. The sandbox constrains. Deterministic
verification proves mutation results. The orchestrator alone decides completion.

## Non-negotiable rules

- Complete only the assigned task file; do not begin dependent or future tasks.
- Inspect Git state and the named files before editing.
- Do not weaken policy, approvals, sandboxing, digest pinning, verification,
  redaction, audit bounds, or capability restrictions.
- Do not add dynamic MCP discovery, arbitrary MCP dispatch, remote MCP, generic
  network access, Git mutation, package installation, shell strings, or
  multi-tenant/concurrent execution.
- Do not add dependencies unless the task explicitly permits it.
- Stop after one focused commit.

## Required preflight

```sh
git status --short
git log --oneline -5
git diff --check
```

## Required completion report

- Commit hash
- Changed files
- Tests added or updated
- Exact verification commands and results
- Ignored/skipped tests and environment dependencies
- Known limitations
- Working-tree status
