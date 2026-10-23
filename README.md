# local-agent-harness

Local, air-gapped agent harness for one repository task at a time.

## Repository history

This repository was initialized during acceptance hardening. The
`pre-endpoint-sandbox` baseline tag is a reconstructed snapshot from the
OpenCode editor snapshot tree `185fe01dccd6817796ce262a4ef85c85fd6b884e`, not
original historical Git state. The `m12-endpoint-sandbox-review` tag marks the
hardened review commit.

## Run

```sh
cargo run
```

The binary validates configuration before entering the terminal UI. Endpoint
defaults and safety rules are documented in `SECURITY.md`. The default
`HARNESS_ENDPOINT_PROFILE=local` accepts only plaintext HTTP on loopback. The
explicit `approved_remote` profile requires the `remote` Cargo feature, an exact
HTTPS origin allowlist, an explicit model name, and a credential-reference
environment variable; Jev remains loopback-only.

Useful environment variables include `HARNESS_ROOT`, `HARNESS_ENDPOINT_PROFILE`,
`HARNESS_LLM_BASE`, `HARNESS_MODEL`, `HARNESS_LLM_ALLOWED_ORIGINS`,
`HARNESS_LLM_CREDENTIAL_ENV`, `HARNESS_JEV_BASE`, `HARNESS_MCP_PROFILE`,
`HARNESS_MCP_ALLOW_UNSANDBOXED`, and the `HARNESS_APPROVAL_*` policy variables
documented below.

To validate static MCP configuration, start the enabled servers, verify their
advertised tools, shut them down, and exit without opening the TUI or starting a
task/model turn:

```sh
cargo run -- --headless
```

## MCP configuration

MCP servers are optional, statically configured, and disabled by default
(`HARNESS_MCP_PROFILE=none`). The harness supports exactly three fixed server
slots: `filesystem`, `git`, and `shell`. Server names, executables, arguments,
working directories, and sandbox wrappers never come from model output.

| Profile | Required servers | Enabled capabilities |
| --- | --- | --- |
| `none` | none | none; tool calls fail closed as unavailable |
| `read_only` | filesystem, git | `read_file`, `git_status`, `git_diff`; `run_command` only when a shell server is explicitly configured |
| `workspace_mutation` | filesystem, git, shell | `read_file`, `write_file`, `apply_patch`, `git_status`, `git_diff`, `run_command` |

`HARNESS_MCP_PROFILE` selects the profile. Each slot uses the same keys with a
`HARNESS_MCP_FS_`, `HARNESS_MCP_GIT_`, or `HARNESS_MCP_SHELL_` prefix:

| Variable suffix | Meaning | Default |
| --- | --- | --- |
| `_EXECUTABLE` | Absolute path to a vetted regular executable file. No PATH lookup or fallback. | unset |
| `_ENABLED` | Explicit boolean override. | true when `_EXECUTABLE` is set |
| `_ARGS` | JSON array of argument strings, for example `["--root","/srv/repo"]`. Never a shell command string. | `[]` |
| `_WORKDIR` | Existing server working directory. | `HARNESS_ROOT` |
| `_PROTOCOL_VERSION` | MCP revision the server implements. | `2025-06-18` |
| `_STARTUP_TIMEOUT_SECS` | Bound for spawn plus initialize and `tools/list`. | `30` |
| `_REQUEST_TIMEOUT_SECS` | Bound for each later request. | `120` |
| `_STDERR_BYTES` | Bounded captured stderr for startup diagnostics. | `16384` |
| `_SANDBOX_MODE` | `none`, `external-wrapper`, or `bubblewrap`. Enabled servers require a non-`none` mode unless `HARNESS_MCP_ALLOW_UNSANDBOXED=true` is explicitly set. | `none` |
| `_SANDBOX_EXECUTABLE` | Absolute wrapper or bubblewrap executable. | unset |
| `_SANDBOX_ARGS` | JSON array fixed wrapper arguments before the server executable and its arguments. Used only by `external-wrapper`. | `[]` |

`HARNESS_MCP_PATH` optionally supplies the child `PATH`; otherwise a minimal
platform default is used. Children never inherit the harness environment. They
receive only an explicit `PATH` and a harness-controlled `HOME`.

Sandboxing is fail-closed for enabled servers. Startup rejects `none` unless
`HARNESS_MCP_ALLOW_UNSANDBOXED=true` is set explicitly; that setting is intended
only for development and testing. A configured bubblewrap executable must be an
absolute regular file and is validated at startup. The default `none` profile
starts no servers, so it does not require the opt-in.

Arguments are bounded to 64 entries of at most 4096 bytes each; strings that
contain NUL are rejected. Configuration and startup are all-or-nothing before
terminal setup or task acceptance. Relative/missing/non-file executables,
malformed JSON arguments, NUL or oversized values, missing required profile servers, a failed handshake,
a malformed tool list, or a capability mismatch fail startup and terminate every
child started by that attempt. Extra tools outside the fixed manifest are
rejected; a profile-disabled fixed capability remains unreachable.

`external-wrapper` runs a fixed administrator-configured wrapper executable with
fixed prefix arguments, then the server executable and its fixed arguments. The
harness does not implement a generic model-controlled command pipeline.

`bubblewrap` runs the fixed server under a harness-managed bubblewrap sandbox on
Linux. The sandbox uses a new network namespace, clears the wrapper environment,
mounts selected system directories read-only, mounts the harness-controlled
`HOME` writable, mounts the workspace read-only for `read_only` and writable for
`workspace_mutation`, and binds the fixed server executable read-only when it is
outside the system mounts. Bubblewrap itself must be installed and configured
by the deployment; the harness fails startup if the configured executable is
missing. In `none` mode the harness performs process hygiene only: it does
**not** provide a network sandbox, and enabled servers are rejected unless the
explicit development opt-in is present. Even with an external wrapper, network
isolation depends on that wrapper and the surrounding OS policy. See
`SECURITY.md`.

## Offline and vendored builds

Prepare dependencies on a controlled, connected machine, then move the
resulting source tree to the target:

```sh
cargo vendor vendor
cargo build --offline
cargo test --offline
```

Review and commit `Cargo.lock` and the intentional `.cargo/config.toml` source
replacement. Keep model files and MCP binaries pinned and checksum-verified.
Offline Cargo only isolates dependency resolution; it does not restrict runtime
network access by the harness, model/Jev endpoints, verification commands, or
MCP child processes. See `SECURITY.md` for the runtime boundary and strict
air-gap controls.

## TUI usage

The first screen is the task-entry prompt. Enter a task, then press `Enter`.
Only one task runs at a time.

| Key | Action |
| --- | --- |
| `Enter` | Submit the task; approve an approval prompt |
| `Esc` | Deny an approval prompt; close help; interrupt is not supported |
| `Ctrl-C` | Request cancellation of the active task; quit when idle |
| `q` | Quit; requests cancellation first while a task is active |
| `Up` / `Down` | Scroll the activity transcript |
| `PageUp` / `PageDown` | Scroll faster |
| `Tab` | Switch focus between activity and detail |
| `?` / `F1` | Toggle the help overlay |

A second `Ctrl-C` exits after the visible cancellation grace period. The UI
renders bounded, redacted event summaries. It never displays raw prompts, raw
tool payloads, hidden reasoning, or complete HTTP bodies.

## Streaming and approvals

Visible assistant text is streamed to the TUI as best-effort display events.
Hidden reasoning is never forwarded. Tool-call deltas, raw argument fragments,
raw prompts, and complete response bodies are never forwarded as stream events.
Only the fully assembled assistant turn is authoritative; streamed deltas are
not appended individually to conversation history. The UI coalesces adjacent
deltas and drops them under bounded-channel backpressure with a visible
truncation warning. A missing or closed UI observer does not fail a model call
or turn a valid task into a false completion.

Approval is an explicit gate after policy authorization and capability
routing, and before MCP dispatch. A policy-denied action never reaches an
approval prompt. `AgentTool::McpTool` remains denied. Approval defaults to
requiring a decision for workspace mutations; read-only file and Git operations
do not require approval. Configure the policy with:

| Variable | Meaning | Default |
| --- | --- | --- |
| `HARNESS_APPROVAL_MUTATIONS` | Require approval for `write_file` and `apply_patch`. | `true` |
| `HARNESS_APPROVAL_RISKY_COMMANDS` | Require approval for configured command programs. | `false` |
| `HARNESS_APPROVAL_RISKY_PROGRAMS` | Comma-separated bare program names for command approval. | `git,cargo,npm,pip,python3` |
| `HARNESS_APPROVAL_TIMEOUT_SECS` | Maximum wait for one decision, from 1 to 3600 seconds. | `120` |

Prompts contain only canonical action metadata: the action type, a
repository-relative path where applicable, a bounded program plus redacted
argument summary for commands, and a concise reason. File contents, patches,
environment values, and raw tool-call JSON are never shown. Every prompt has a
unique action ID. `y`/`Enter` approves only that displayed ID and `n`/`Esc`
denies only that ID; arbitrary text is not an approval. Stale, duplicate, or
mismatched decisions are ignored. Timeout, denial, channel closure, and
cancellation never execute the action. The model receives a bounded
`APPROVAL_DENIED` or `APPROVAL_EXPIRED` tool result. The TUI only returns the
decision; the orchestrator performs any approved call.

Without an interactive approval gate, approval-required actions fail closed.
This includes headless/library orchestration; there is no auto-approval and no
fallback that widens policy or capabilities.

## Trust boundary

The TUI is a presentation and input layer. It does not execute tools, call the
model or Jev directly, change policy, bypass approval or verification, or mark a
task complete. Those decisions remain in the headless orchestrator and the
existing policy/verification layers.

## Terminal recovery

Terminal state is restored on normal exit, errors, cancellation, and panics. If
a terminal is left in raw mode or the alternate screen by an abnormal host
failure, run:

```sh
reset
```
