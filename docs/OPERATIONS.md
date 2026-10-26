# Operator setup and recovery

This runbook is for the local-first deployment. The harness is intentionally
single-task and local: one operator, one repository, one task at a time. It does
not commit, push, install packages, or discover endpoints.

## Prerequisites

- A fixed Python 3 interpreter at an absolute path such as
  `/usr/bin/python3`. PATH lookup is not used for helper execution.
- Reviewed helper scripts installed at a canonical directory outside the
  project root. The active read-only inventory is `read_file.py`,
  `git_status.py`, and `git_diff.py`; `write_file.py` enables approved file
  creation and digest-guarded replacement. Change remains gated until the
  reviewed verifier script exists.
- Rust toolchain, `cargo`, and the verifier tools required by the target
  repository (`rustfmt` for Rust projects).
- A loopback OpenAI-compatible model server, normally `llama-server` on
  `http://127.0.0.1:8080`.
- Optionally, a loopback Jev System 1 service. Jev is advisory; an unavailable
  Jev degrades to conservative routing and does not authorize anything.

Bubblewrap and fixed MCP executables are only needed if you explicitly run the
frozen MCP reference/demo path. The normal TUI and headless workflows do not
start MCP and do not require MCP profiles, executables, digests, or sandbox
startup.

Build and test on the target or prepare an offline/vendored build as described
in the main README. Keep `Cargo.lock`, reviewed helper scripts, and model files
pinned.

## Minimal production-safe configuration

The default execution backend is the fixed script substrate. Investigate is the
first enabled workflow. Change remains gated until the reviewed
`run_verifier.py` script is installed.

```sh
export HARNESS_ROOT=/srv/harness/workspace
export HARNESS_ENDPOINT_PROFILE=local
export HARNESS_LLM_BASE=http://127.0.0.1:8080
export HARNESS_MODEL=local-model
export HARNESS_JEV_BASE=http://127.0.0.1:9090

export HARNESS_SCRIPT_ROOT=/opt/harness/scripts
export HARNESS_SCRIPT_INTERPRETER=/usr/bin/python3
# Optional; default is 60 seconds and the maximum is 3600.
# export HARNESS_SCRIPT_TIMEOUT_SECS=60

# Default is <root>/.harness-audit/audit.jsonl.
# export HARNESS_AUDIT_PATH=/var/lib/harness/audit.jsonl
```

If `HARNESS_SCRIPT_ROOT` is omitted, the harness looks for `scripts/` beside the
executable, in the installed share directory, then in the working directory.
Startup still canonicalizes the selected directory and requires it to remain
outside the project root. No MCP configuration is required.

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

Validate configuration and the active script-runtime composition before opening
the terminal UI:

```sh
cargo run -- --headless
```

Expected output includes the canonical project root, script root, interpreter,
audit path, each resolved script mapping, and whether Change is available or
gated. A failure here means no task was accepted. This preflight never starts
MCP. Then start the TUI:

```sh
cargo run
```

Use a disposable repository for the first live task. Investigate is selected by
default when Change is gated. Confirm the audit file and evidence brief exist
before installing the verifier script.

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

Existing targets and the nearest existing ancestor of a proposed new file are
resolved through symlinks. In-root symlinks are allowed only when the resolved
target remains inside the canonical root and is not sensitive. Escaping, broken,
looping, or otherwise unresolvable symlink paths fail closed with
`PATH_SYMLINK_ESCAPE`; resolved sensitive aliases fail with `PATH_SENSITIVE`.
Policy errors use stable `PATH_*` codes and bound the echoed path to 256 bytes.

## Read-file helper

`scripts/read_file.py` reads a repository-relative path and accepts optional
1-based inclusive `line_start` and `line_end` parameters. It only returns
selected content that decodes as UTF-8; invalid selected content fails with
`READ_NOT_UTF8`. Path authorization, sensitive-name denial, and symlink-escape
rejection use the shared policy before the target is opened. Directories and
other non-regular targets fail with `READ_NOT_FILE`. The helper never mutates
the file.

Successful protocol data has a stable shape: `path`, `content`, `line_start`,
`line_end`, `next_line`, `truncated`, and `partial_line`. `line_start` and
`line_end` identify the first and last source lines present in `content`, or are
`null` when no lines were selected. Content is capped at 64 KiB. When the cap
interrupts a larger selection, `truncated` is `true` and `next_line` is the next
source line to request. Content normally ends at a line boundary; if one selected
source line is larger than the cap, a UTF-8-safe prefix is returned with
`partial_line` set. The unreturned remainder of that line is not addressable by
line range.

## Git status helper

`scripts/git_status.py` runs only a fixed `git status --porcelain=v1
--untracked-files=normal -- .` command in the configured root. It disables the
pager, sets `GIT_OPTIONAL_LOCKS=0`, ignores system and global Git configuration,
disables terminal prompts, and passes a minimal environment. Repository paths
are quoted by Git, and control characters are replaced before output. The helper
does not refresh the index or mutate the worktree, index, refs, remotes, or Git
configuration.

Successful protocol data contains `clean`, `status`, and `truncated`. Status is
capped at 64 KiB and a capped result is marked `truncated`. Clean repositories
return an empty `status` with `clean: true`. A root that is not inside a Git
repository returns the helper error `NOT_A_GIT_REPOSITORY`.

## Git diff helper

`scripts/git_diff.py` requires a boolean `staged` parameter and runs only one
fixed command in the configured root: `git diff` when `staged` is `false`, or
`git diff --cached` when `staged` is `true`. Both forms are scoped to the root
and disable the pager, external diff drivers, text conversion, color, terminal
prompts, and optional locks. The helper uses the same minimal environment and
system/global configuration isolation as the Git status helper. It does not
inspect history, include untracked files, or mutate the worktree, index, refs,
remotes, or Git configuration.

Successful protocol data contains `staged`, `diff`, and `truncated`. Diffs are
capped at 64 KiB. A truncated diff preserves the beginning and end of the Git
output with a `[diff truncated]` marker between them and sets `truncated` to
`true`. Empty diffs return an empty `diff`. A root that is not inside a Git
repository returns the helper error `NOT_A_GIT_REPOSITORY`.

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

## Read-only dispatch

Read-only task catalogs expose exactly `read_file`, `git_status`, and
`git_diff`. The orchestrator rejects any other tool request before policy,
approval, or dispatch, so a model cannot obtain an approval prompt for a write,
verifier, or mutation action that its catalog does not expose. Read-only
actions still pass the shared path and command policy before the script runner
is called.

The script-backed read-only executor maps the strict canonical action into a
fixed `ActionId` and action-specific parameters. It does not accept script
paths, interpreter paths, working directories, or shell strings from the model.
Successful helper data is rendered as bounded text for the model; the
orchestrator applies its configured tool-output cap and secret redaction before
the result enters conversation context. Every dispatched action emits
tool-requested/tool-finished events and, when audit is configured, a bounded
`script_finished` record.

Cancellation drops the in-flight runner future; the helper child is configured
with `kill_on_drop`, so cancellation cannot leave a successful late result in
the task or mutate repository content. Model-visible tool errors include
teaching errors for malformed action arguments, policy denials, and bounded
helper failures.

## Investigate mode

The operator selects `Investigate` before submitting a task. In the TUI, press
`F2` while the task-entry field is focused to cycle through `change`,
`investigate`, and `plan`; the header and prompt show the selected mode. A mode
is fixed for the lifetime of the task and cannot be changed by the model.

Investigate tasks use the read-only transition path `Learn -> Review ->
Compile`. Plan, Act, and Verify are unavailable, so no mutation can be approved
or dispatched. The model sees only `read_file`, `git_status`, and `git_diff`.

Completion requires a durable, bounded, redacted evidence brief at:

```text
$HARNESS_ROOT/.ai-wiki/evidence/<task-id>.md
```

The brief separates harness-confirmed facts (successful read-only actions,
inspected files, and observed Git state) from model hypotheses, records missing
evidence and uncertainty, and includes a recommended next action. If model
findings are unavailable or the model call fails, the deterministic compiler
fallback still writes the brief from the recorded action trace. Cancellation
before compilation leaves no brief and produces a cancelled terminal state, not
completion.

The TUI and `--headless` both construct the same script runtime: canonical path
policy, audit sink, fixed `ScriptRunner`, and `ScriptToolExecutor` injected into
the orchestrator as a `ToolExecutor`. Investigate tasks dispatch through that
script runner; no MCP runtime, server, profile, digest, or Bubblewrap startup is
required.

The TUI shows Change as gated when `run_verifier.py` is not installed. The
worker also rejects a Change submission defensively. MCP is never selected as
an automatic fallback. The script inventory and installed location are resolved
at startup; release packaging and checksum verification remain separate release
work.

## Plan mode

Plan mode is a no-mutation planning workflow using the transition path
`Learn -> Plan -> Review -> Compile -> Completed`. The model may gather bounded
repository evidence with only `read_file`, `git_status`, and `git_diff`, then
must return a structured plan. Plan never enters Act or Verify, cannot mutate
task/source files, cannot run a verifier, and never starts Change mode. The only
write is the harness-owned plan artifact under `.ai-wiki/plans/`.

The deterministic review step requires non-empty sections for the goal,
repository evidence used, likely files, ordered steps, assumptions, risks,
expected verifier profile, blockers/missing evidence, and confirmed facts
versus model inferences. Invalid or missing model output produces a bounded,
redacted deterministic fallback artifact rather than a false "valid" plan.

Plan artifacts are written to:

```text
$HARNESS_ROOT/.ai-wiki/plans/<task-id>.md
```

The harness detects the verifier profile through the existing deterministic
detection path but does not execute it in Plan mode. The artifact records the
harness-selected commands as authoritative and ignores any model-proposed
commands. If no supported profile is detected, the artifact records an explicit
blocker and the task ends blocked; the harness never invents verification
commands. Starting implementation requires a separate operator-submitted Change
task after reviewing the plan.

## Write-file schema preflight

Change-mode `write_file` requests pass a strict schema gate before the shared
path policy, capability check, or approval prompt. The validated form is
`WriteFile { path, content, expected_sha256 }`:

- `path` must be a non-empty repository-relative path of at most 256 bytes,
  without NUL or other control characters, and must not escape the project root.
- `content` must be UTF-8 text of at most 49,152 bytes and must not contain NUL.
- `expected_sha256`, when present, must be exactly 64 lowercase hexadecimal
  characters. Upper-case or prefixed digests are rejected. A missing target may
  be created without one; an existing target cannot be replaced without it.
- `write_file` is rejected in Investigate and Plan modes before approval. A
  denied path or invalid schema never creates an approval prompt or dispatches
  an action.
- Approval metadata includes only the bounded target path and reason; it never
  includes file content or the expected digest.

Approved Change-mode `write_file` requests are dispatched to the fixed
`write_file.py` helper. The helper revalidates the target and parent through the
Python path policy, writes a temporary sibling, and publishes the result
atomically. If the target does not exist, it is created with an exclusive hard
link. If it exists, it must be a regular non-symlink file and the request must
supply `expected_sha256` matching the target's current content; otherwise the
write fails without modifying the target. Matching overwrites use an atomic
replacement and preserve ordinary permission bits where practical. Missing
parent directories inside the permitted project tree are created for new files;
on failure, the temporary file and any directories created for that request are
removed.

The helper rechecks the target identity and metadata while hashing and again
immediately before replacement. A file that appears, disappears, changes type,
or changes content after the caller computed the digest is rejected rather than
overwritten. This narrows the check-to-replace race; it is not a filesystem
transaction or a defense against an attacker who can already modify the
repository concurrently.

Request content is limited to 49,152 UTF-8 bytes and the same 256-byte path
bound as preflight. The helper independently enforces both bounds. Successful writes
report only the normalized path, byte count, creation flag, new lowercase
SHA-256 digest, and, for replacement, the previous digest. Audit
`ScriptFinished` metadata records those safe fields and never file content. A
successful write marks prior verification stale and the orchestrator must run
harness-selected verification before review and completion.

## Audit evidence

Audit records are append-only JSON Lines. The default location is:

```text
$HARNESS_ROOT/.harness-audit/audit.jsonl
```

Each record is bounded to 16 KiB. Records include task/phase transitions, plan
summary, tool classification, fixed script identity and action outcomes,
approval decisions, verification outcomes, Wiki compilation, evidence brief
compilation, and plan artifact compilation. Raw tool payloads, file contents,
prompts, hidden reasoning, and credentials are not recorded in tool audit
records. The frozen MCP adapter, if
run explicitly, would also record local executable provenance; it is not part of
the normal startup path. Verification output is bounded and redacted, but it is
still local evidence and should be treated as sensitive.

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

The deterministic reference demo exercises the frozen MCP adapter with real
Bubblewrap-contained processes, approvals, Cargo verification, audit, and Wiki
compilation against temporary fixtures. It is not the active P0 execution path;
use `--headless` and the TUI to validate the script substrate:

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

### Orphaned helper-script processes

The script runner uses `kill_on_drop`, hard timeouts, and explicit reaping, but a
host crash can leave a helper behind. Inspect only the expected script names:

```sh
pgrep -af 'read_file.py|git_status.py|git_diff.py'
```

Terminate only specific orphaned processes after confirming they belong to the
harness. Do not use a broad `pkill` pattern on a shared host. If you explicitly
run the frozen MCP demo, inspect only its documented mock server executable.

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

Verification commands run as the harness account, outside the helper-script
boundary. Run the harness under a dedicated account or container with the same
filesystem and network policy you intend for verifier commands.

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
- The active script path does not use Bubblewrap; helper scripts are constrained
  by the fixed inventory and path policy, not an OS sandbox. Verifier commands
  and the harness itself run outside the helper-script boundary.
- Remote/work activation is a separate administrative decision and is not part
  of the local-first profile.
