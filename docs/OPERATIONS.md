# Operator setup and recovery

This runbook is for the local-first deployment. The harness is intentionally
single-task and local: one operator, one repository, one task at a time. It does
not commit, push, install packages, or discover endpoints.

## Prerequisites

- Linux with Bubblewrap (`bwrap`) installed and usable by the harness account.
- Rust toolchain, `cargo`, and the verifier tools required by the target
  repository (`rustfmt` for Rust projects).
- A loopback OpenAI-compatible model server, normally `llama-server` on
  `http://127.0.0.1:8080`.
- Optionally, a loopback Jev System 1 service. Jev is advisory; an unavailable
  Jev degrades to conservative routing and does not authorize anything.
- Fixed, reviewed MCP executables for the `filesystem`, `git`, and `shell`
  slots. Do not use PATH lookup, downloaded binaries, or model-selected paths.

Build and test on the target or prepare an offline/vendored build as described
in the main README. Keep `Cargo.lock`, MCP binaries, and model files pinned.

## Minimal production-safe configuration

Start with `read_only`, then enable mutation only when required.

```sh
export HARNESS_ROOT=/srv/harness/workspace
export HARNESS_ENDPOINT_PROFILE=local
export HARNESS_LLM_BASE=http://127.0.0.1:8080
export HARNESS_MODEL=local-model
export HARNESS_JEV_BASE=http://127.0.0.1:9090

export HARNESS_MCP_PROFILE=read_only
export HARNESS_MCP_FS_EXECUTABLE=/opt/harness/bin/filesystem-mcp
export HARNESS_MCP_FS_SANDBOX_MODE=bubblewrap
export HARNESS_MCP_FS_SANDBOX_EXECUTABLE=/usr/bin/bwrap
export HARNESS_MCP_GIT_EXECUTABLE=/opt/harness/bin/git-mcp
export HARNESS_MCP_GIT_SANDBOX_MODE=bubblewrap
export HARNESS_MCP_GIT_SANDBOX_EXECUTABLE=/usr/bin/bwrap

# Pin the reviewed executable digests. Compute them from the canonical files.
export HARNESS_MCP_APPROVED_DIGESTS='{"filesystem":"<64-hex>","git":"<64-hex>"}'

# Default is <root>/.harness-audit/audit.jsonl.
# export HARNESS_AUDIT_PATH=/var/lib/harness/audit.jsonl
```

Do not set `HARNESS_MCP_ALLOW_UNSANDBOXED=true` in production. It is a visibly
unsafe development/testing exception and is rejected for enabled servers unless
explicitly present.

Useful bounds:

```sh
export HARNESS_MAX_ACT_TURNS=20
export HARNESS_MAX_TOOL_CALLS=60
export HARNESS_MAX_VERIFY_RETRIES=2
export HARNESS_MAX_WALL_SECS=1800
export HARNESS_MAX_TOOL_OUTPUT_BYTES=4000
export HARNESS_APPROVAL_MUTATIONS=true
export HARNESS_APPROVAL_TIMEOUT_SECS=120
```

`HARNESS_AUDIT_PATH` must name a file, must not be the repository root, and must
be outside `.ai-wiki/`. Relative paths are resolved under `HARNESS_ROOT` and
must not escape it. A path inside the repository must be a file under
`.harness-audit/`; an absolute path outside the repository is allowed. The
`.harness-audit/` directory is denied to model file tools, so audit evidence is
local operator evidence rather than model context. Symlinks that resolve into
`.ai-wiki/` or outside the repository are rejected.

## Preflight and first run

Validate configuration and the full MCP startup/shutdown path before opening the
terminal UI:

```sh
cargo run -- --headless
```

Expected output includes the profile, number of started servers, Bubblewrap
warnings, and any configuration error. A failure here means no task was
accepted. Then start the TUI:

```sh
cargo run
```

Use a disposable repository for the first live task. Start with a read-only
inspection task. Confirm the audit file exists before enabling
`workspace_mutation`.

## Path policy

The configured project root is canonicalized once when the shared path policy is
created. Model-supplied file paths must be repository-relative: absolute paths,
empty paths, NUL bytes, lexical root traversal, paths outside the canonical root,
and sensitive or harness-private targets are denied before approval or dispatch.

The shared policy is implemented by `src/path_policy.rs` for the orchestrator
and `scripts/path_policy.py` for fixed helper scripts. It is used for both
existing-path and proposed new-file checks. It denies credential names and
directories (for example `.env`, `.ssh`, `credentials.json`, and key material)
plus the harness-private `.harness-audit/` and `.ai-wiki/` trees. Matching is by
path component, file name, and extension, not by substring over the whole path.
Policy errors use stable `PATH_*` codes and bound the echoed path to 256 bytes.

## Script runtime boundary

The fixed helper-script runner is the only script execution path. It launches a
reviewed script by canonical path and argv, with no shell and no model-selected
executable or working directory. The default hard timeout is 60 seconds; the
maximum accepted timeout is 3,600 seconds. Timeout and cancellation explicitly
terminate and reap the child, so a timed-out or cancelled action cannot report
success.

Helper scripts do not inherit the harness environment. They receive only a
controlled `PATH`, a harness-owned temporary `HOME` (mode `0700`, removed after
the run), locale variables, and a temporary directory where applicable
(`LANG`, `LC_ALL`, `TMPDIR` on Unix; `USERPROFILE`, `TEMP`, `TMP` on Windows).
Proxy variables, cloud credentials, model credentials, and arbitrary ambient
variables are absent by construction.

When invoked through the audited execution path, a `script_finished` record
contains the action ID, fixed script file name, canonical script identity,
success flag, bounded result classification, duration, and exit code. It does not
contain request parameters, stdout data, stderr diagnostics, or environment
values.

## Audit evidence

Audit records are append-only JSON Lines. The default location is:

```text
$HARNESS_ROOT/.harness-audit/audit.jsonl
```

Each record is bounded to 16 KiB. Records include task/phase transitions, plan
summary, tool classification, script action outcomes, approval decisions,
verification outcomes, Wiki compilation, and local MCP provenance (canonical
executable path, SHA-256, and reported version). Raw tool payloads, file
contents, prompts, hidden reasoning, credentials, and complete HTTP/MCP bodies
are not recorded in tool audit records. Verification output is bounded and
redacted, but it is still local evidence and should be treated as sensitive.

Inspect evidence with:

```sh
audit="$HARNESS_ROOT/.harness-audit/audit.jsonl"
jq -c '{timestamp,task_id,kind,message}' "$audit" | tail -40
jq -c 'select(.kind=="task_finished") | {timestamp,task_id,metadata}' "$audit"
```

Retention is operator-owned. Preserve evidence for incident review; otherwise
rotate or archive it according to local policy. Do not edit an existing audit
file in place.

## Repeatable demonstration

The deterministic production-safe demo runs real Bubblewrap-contained MCP
processes using the repository's mock MCP test binary in real-filesystem mode,
plus approvals, Cargo verification, audit, and Wiki compilation against temporary
fixtures:

```sh
./demo/run.sh
```

Evidence is written under `target/demo-evidence/run-*`. See
[`../demo/README.md`](../demo/README.md). The demo uses a scripted model to stay
repeatable; use the TUI with the local model for a live smoke test.

## Recovery

### Terminal is left in raw mode or the alternate screen

Run:

```sh
reset
```

Terminal state is restored on normal exit, errors, cancellation, and panics. A
host failure can still leave the terminal in a bad state.

### Stop an active task

- `Ctrl-C` requests cancellation; a second `Ctrl-C` exits after the visible
  grace period.
- `q` requests cancellation and quits.
- An approval prompt can be denied with `Esc`; it also expires after the
  configured approval timeout.
- Cancellation never converts into completion. Review the audit terminal
  record before restarting.

### Orphaned MCP processes

The harness uses `kill_on_drop` and bounded shutdown, but a host crash can leave
children behind. Inspect only the expected executables:

```sh
pgrep -af 'filesystem-mcp|git-mcp|shell-mcp'
```

Terminate only the specific orphaned processes after confirming they belong to
the harness. Do not use a broad `pkill` pattern on a shared host.

### Audit write failure

A task fails rather than completing if audit persistence fails. Check:

```sh
ls -ld "$(dirname "$HARNESS_AUDIT_PATH")"
df -h "$(dirname "$HARNESS_AUDIT_PATH")"
```

Fix permissions, disk space, or the configured path, then rerun the task.
Preserve any partial evidence file for review; do not delete it merely because a
write failed.

### Verification failure or blocked task

The audit `verification_finished` record contains the bounded failure tail. A
failed verification returns the task to `Act` only within the retry budget; when
the budget is exhausted the task becomes `Blocked`. Do not claim completion
manually. Inspect the workspace diff, fix the cause, and start a new task.

Verification commands run as the harness account, outside the MCP sandbox. Run
the harness under a dedicated account or container with the same filesystem and
network policy you intend for verifier commands.

### Wiki load or compile failure

Wiki failure never produces a false completion. If `.ai-wiki/` is malformed,
preserve it for diagnosis and restore the last known-good copy before restarting.
Do not hand-edit the active state file while a task is running.

### Model or Jev unavailable

The LLM client fails explicitly; there is no remote fallback. Check that the
configured loopback endpoint is listening and that the model name matches.
Jev failures are advisory fallbacks and should not be treated as task success.

### Suspected secret exposure or prompt injection

1. Stop the task and quit the harness.
2. Preserve `.harness-audit/` and the workspace diff.
3. Rotate any credential that may have been exposed. The harness never stores
   the credential value, but the local model and tool output remain untrusted.
4. Review the audit for policy denials, approvals, and verification outcomes.
5. Do not copy raw tool output or model output into prompts or new tasks without
   review.

### Rollback

The harness does not run `git commit`, `git push`, package installation, or
destructive Git operations. Workspace changes remain operator-owned. Review and
roll back with normal repository tools:

```sh
git status
git diff
git restore -- <path>
```

## Known limits

- The demo's model is scripted; it proves harness wiring, not live model quality.
- Audit retention and rotation are operator responsibilities.
- The harness enforces one task at a time, but it is not a multi-tenant service.
- Bubblewrap contains MCP children; verifier commands and the harness itself run
  outside that sandbox.
- Remote/work activation is a separate administrative decision and is not part
  of the local-first profile.
