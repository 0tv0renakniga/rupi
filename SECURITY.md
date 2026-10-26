# Security model

## Approved endpoints and data egress

The harness is local-first and air-gapped by default. The current runtime opens
HTTP connections only from the System 2 LLM client and the Jev System 1 adapter.
There is no telemetry, analytics, crash reporting, update check, remote
configuration, endpoint discovery, proxy-based rerouting, or automatic cloud
fallback.

The only endpoints the harness may use are explicitly configured:

1. A local `llama.cpp` / `llama-server` endpoint under the default `local`
   profile, normally loopback HTTP such as `http://127.0.0.1:8080`.
2. A local Jev System 1 endpoint, always loopback HTTP such as
   `http://127.0.0.1:9090` (the configured default).
3. An approved production work API under the explicit `approved_remote` profile,
   only when the `remote` Cargo feature is enabled and all remote settings are
   deliberately supplied in configuration.

Bounded task context, prompts, model output, and tool-derived data can be sent
to these configured endpoints. Jev receives only compact derived values
supplied by its caller. Model and tool output is untrusted and must not be
treated as instructions to expand this endpoint set.

### Endpoint rules

- `HARNESS_ENDPOINT_PROFILE=local` is the default. It accepts only plaintext
  HTTP LLM endpoints whose host is loopback (`localhost`, `127.0.0.0/8`, or
  `::1`) and rejects remote-only allowlist/credential settings.
- `HARNESS_ENDPOINT_PROFILE=approved_remote` requires the `remote` Cargo
  feature, `HARNESS_MODEL`, a non-empty exact-origin
  `HARNESS_LLM_ALLOWED_ORIGINS` list, and `HARNESS_LLM_CREDENTIAL_ENV`.
- Approved-remote LLM bases must be bare `https://` origins with no path, query,
  fragment, user information, or wildcard. The base origin must exactly match
  one allowlisted origin.
- The credential-reference variable holds the token; its value is read at
  startup, sent only as a bearer header, and never stored in configuration,
  audit records, logs, or `Debug` output.
- Jev is always a local service: non-loopback or TLS Jev endpoints are rejected
  in every profile, including `remote`.
- Enabling `remote` does not select a profile, configure, contact, or upload data
  to any service. There is no default remote URL and no endpoint discovery.
- URLs containing credentials are rejected. Logs retain only the scheme, host,
  and explicit port; paths, query strings, fragments, and user information are
  never logged.
- The two profiles are mutually exclusive and there is no automatic fallback.
  Model/work API failures surface explicitly; Jev failures use the adapter's
  documented safe fallbacks (`Escalate`, `Uncertain`, or `None`).

## System 2 LLM client

The OpenAI-compatible client is an explicitly configured endpoint. The default
`local` profile restricts it to plaintext HTTP on a loopback host. The
`approved_remote` profile permits only an allowlisted HTTPS origin with an
explicit model and credential reference. The adapter accepts no per-request URL,
host, scheme, path, redirect target, credential, or proxy configuration from
callers; an optional bearer credential comes only from the configured
environment-variable reference.

- Requests use only the fixed path `/v1/chat/completions`, appended to the
  validated configured base. Base URLs with a query string or fragment are
  rejected.
- The model name must be supplied explicitly in configuration. It is never
  inferred from a GGUF path or server response.
- Redirects are disabled, environment proxy settings are ignored, and requests
  have bounded connect and total timeouts suitable for local inference.
- Message content, tool definitions, tool-call counts, tool names, accumulated
  tool arguments, request bodies, SSE lines, streamed bytes, and assembled
  responses are bounded. Limit violations fail explicitly rather than silently
  truncating prompts or tool arguments.
- Streaming accepts only the documented SSE framing and requires the
  `[DONE]` terminal marker. Malformed data frames, invalid UTF-8, and premature
  EOF are protocol errors.
- Request and response bodies and the bearer credential are not logged. Debug
  output for the adapter exposes only a scheme/host/port-safe endpoint and
  whether a credential is configured.
- Remote settings do not create a second protocol implementation: streaming,
  parsing, bounds, redaction, and tool-call handling remain in the single
  OpenAI-compatible adapter.
- Tool calls returned by the model are untrusted proposals. This adapter does
  not execute them, validate their arguments semantically, or grant
  permissions. Policy, MCP authorization, and budgets remain the harness's
  responsibility.
- `complete_text` is text-only, sends no tools, rejects tool-call output, and
  never substitutes for deterministic verification.
- If the configured LLM endpoint is unavailable, the client fails explicitly.
  It never retries against a different host, proxy-derived route, cloud service,
  or fallback endpoint.

### llama-server CORS and local authentication

A typical local `llama-server` may warn that CORS allows all origins and that no
API key is configured. The harness is a native client and does not depend on
CORS, browser access, or an API key. It must not expose the model endpoint to a
browser or other untrusted local processes. Strict deployments should bind the
server to loopback only, restrict CORS to trusted origins where the server
supports it, add local authentication where supported, and enforce the loopback
network boundary with host firewall or process controls. Do not assume specific
llama.cpp option names across versions.

## Jev System 1 adapter

Jev is an explicitly configured approved endpoint. In the default build it is
restricted to plaintext HTTP on a loopback host; non-loopback and TLS endpoints
still require the explicit `remote` feature. The adapter accepts no per-request
URL, host, scheme, path, redirect target, authentication setting, or proxy
configuration from callers.

- Requests use only the fixed paths `/v1/choice`, `/v1/noul`, and `/v1/score`,
  appended to the validated configured base.
- Redirects are disabled, environment proxy settings are ignored, and requests
  have short bounded connect and total timeouts.
- Prompt, option, claim, evidence, query, and document inputs are bounded and
  UTF-8-safely truncated. Complete request and response bodies have byte limits.
- Request and response bodies are not logged at normal logging levels. Debug
  output for the adapter exposes only a scheme/host/port-safe endpoint.
- Jev receives bounded derived task or Wiki context such as choice prompts and
  options, a completion claim with evidence, or a ranking query with documents.
  It does not receive raw tool output, environment dumps, or credential files
  through this adapter. Callers must still avoid placing secrets in the derived
  values they send.
- Jev outputs are advisory. They cannot authorize tools, bypass `Policy`,
  execute tool calls, claim verification passed, or transition a task to
  completed. A Jev `Pass` never substitutes for a real verifier exit code.

These controls limit the harness's intended data flows. They do not provide
confidentiality against a compromised local service or a host-level attacker;
strict deployments still depend on network, firewall, process, and filesystem
controls.

## Headless orchestration boundary

The M8 orchestrator owns task state, budgets, context, transitions, and terminal
outcomes. System 2 and Jev remain untrusted advisory components: neither can
authorize a tool, bypass `Policy`, execute a tool call directly, or declare a
task complete.

- Change mode's completion path is `Learn -> Plan -> Act -> Verify -> Review ->
  Compile -> Completed`. A mutation invalidates prior verification and forces
  another `Verify`. Missing, failed, or unusable verification blocks
  completion.
- Investigate mode uses `Learn -> Review -> Compile -> Completed`; Plan, Act, and
  Verify are unavailable, so no mutation can be approved or dispatched.
- Plan mode uses `Learn -> Plan -> Review -> Compile -> Completed`. Its Plan
  phase is limited to read-only evidence gathering and structured plan
  generation; Act and Verify are unavailable. The harness writes a bounded,
  redacted advisory plan artifact, detects but never runs the verifier, and
  records missing verifier support as a blocker. Plan cannot start Change mode.
- Tool calls are parsed into canonical `AgentTool` values before dispatch.
  Malformed arguments are returned to the model as bounded teaching feedback and
  are not sent to the dispatcher. Write requests additionally require a strict
  Change-mode schema preflight: a bounded relative path, bounded NUL-free UTF-8
  content, and an optional 64-character lowercase SHA-256 digest. Schema or
  path denial occurs before capability checks and approval; approval metadata
  never contains file content. Approved Change-mode writes dispatch only to the
  fixed write helper, which revalidates the target and parent, writes a temporary
  sibling, and publishes atomically. New targets are created exclusively.
  Existing regular targets require a matching expected SHA-256 computed from
  current content; the helper rechecks identity and metadata before atomic
  replacement, rejects symlinks and mismatches, and preserves ordinary
  permission bits where practical. Successful write audit metadata is limited to
  the normalized path, byte count, creation flag, new digest, and previous
  digest for replacement; file content is never persisted in audit or approval
  data.
- Context compaction preserves the system policy, task objective, plan, and
  verification evidence while dropping or bounding older messages. Tool output,
  plan text, review evidence, audit records, and event payloads are bounded and
  redacted before persistence or emission. Tool audit and event summaries
  classify the outcome and do not embed raw tool payloads or file contents;
  verification tails are bounded, redacted local evidence. Hidden reasoning is
  never added to the conversation, Wiki trace, audit, or events.
- Cancellation is checked around model, tool, verifier, Wiki, and Jev awaits.
  Cancellation cannot be converted into completion.
- Audit failures surface as errors rather than allowing a false completion.
- This boundary does not isolate processes or the network. Strict deployments
  must still enforce filesystem and network isolation externally.

## Audit evidence

The TUI attaches an append-only `AuditLog` before terminal setup. The default
path is `<root>/.harness-audit/audit.jsonl`; `HARNESS_AUDIT_PATH` may select
another file. The path must not be the repository root and must be outside
`.ai-wiki/`. A path inside the repository must be a file under `.harness-audit/`;
a relative path must not escape the repository root. The `.harness-audit/`
directory is denied to model file tools, so provenance and lifecycle evidence
are not exposed to the model through the filesystem capability. Symlinks that
resolve into `.ai-wiki/` or outside the repository are rejected.

- Each serialized record is bounded to 16 KiB. An oversized record fails the
  audit write rather than being silently truncated.
- Tool records contain the canonical tool name, outcome classification, and
  bounded identifiers. They do not contain raw tool output, file contents,
  patches, prompts, hidden reasoning, credentials, or complete HTTP/MCP bodies.
- Verification records contain bounded, redacted tails. Treat them as sensitive
  local evidence because verifier output can include repository details.
- Audit failures surface as task errors and cannot allow false completion.
- The audit file is local evidence, not prompt context. Retention, backup, and
  rotation are operator responsibilities; do not edit evidence in place.

The TUI and the `--headless` startup check use the same script-runtime
composition. Both validate the canonical project root, reviewed script root,
absolute interpreter, audit path, and script inventory before a task can start;
neither starts MCP in the normal workflow. The script executor and audit sink are
injected into the orchestrator as shared runtime dependencies.

## TUI boundary

The Ratatui/crossterm UI is a presentation and input layer, not an
authorization surface.

- The UI never executes tools, dispatches tool calls, calls the model or Jev
  directly, changes `Policy`, or marks a task complete.
- Approval prompts may request an explicit user decision, but approval cannot
  authorize a policy bypass, skip verification, or widen the fixed capability
  manifest.
- The UI renders only bounded, redacted orchestration event summaries. It does
  not render system prompts, raw Wiki contents, raw tool payloads, hidden
  reasoning, credentials, or complete HTTP/MCP bodies.
- Typed task input and streamed model text are not written to stderr by default
  and are never added to the Wiki, audit log, or another persistent transcript
  by the TUI.
- Worker/UI channels are bounded. Critical lifecycle events are retried;
  best-effort stream deltas may be coalesced or dropped only with a visible
  truncation indicator. A closed worker channel becomes a visible terminal
  error rather than a false completion.
- Only normal visible assistant text is eligible for streaming. Hidden
  reasoning and raw tool-call argument deltas are never forwarded. Streamed
  text is observational: the fully assembled assistant turn remains
  authoritative and individual deltas are never appended to conversation
  history. A missing observer or dropped delta cannot fail a model call.
- Approval prompts contain bounded, redacted action metadata only: canonical
  action type, repository-relative path where applicable, a safe program and
  redacted argument summary for commands, and a concise reason. They never
  contain file contents, patches, environment dumps, secrets, or raw tool JSON.
- Approval is checked only after `Policy` authorizes the action and fixed
  capability routing succeeds. Policy-denied actions never reach approval;
  approval cannot bypass policy or widen capabilities. `AgentTool::McpTool`
  remains denied.
- Each approval prompt is bound to one unique action ID. Stale, duplicate, or
  mismatched decisions are ignored. The TUI sends a decision only; it never
  dispatches the approved tool.
- Approval waits have a configured bound. Denial, expiry, cancellation, channel
  closure, or the absence of an interactive gate never execute the action and
  return a bounded model-readable `APPROVAL_DENIED` or `APPROVAL_EXPIRED`
  outcome. Headless/library orchestration fails closed and never auto-approves.
- The default policy requires approval for workspace mutations and does not
  require it for read-only file or Git operations. Command approval is
  independently configurable for a static list of bare program names.
- Cancellation is requested through the existing cancellation token. Terminal
  state is restored on normal exit, errors, cancellation, join failure, and
  panic. A second Ctrl-C after the visible grace period may exit only after
  orderly cleanup begins.

## Frozen MCP process boundary

The normal TUI and `--headless` workflows do not start MCP. The following
controls apply only if the frozen MCP reference adapter or its tests/demo are
invoked explicitly. MCP child binaries and any external sandbox wrapper are an
untrusted process and supply-chain boundary. Enabled servers require
`external-wrapper` or `bubblewrap` by default; startup rejects `none` unless
`HARNESS_MCP_ALLOW_UNSANDBOXED=true` is set explicitly for development or
testing. In that unsandboxed mode the harness starts the configured server
directly, without a shell, using an absolute executable path. That configured
absolute path is the administrator-approved location; the harness canonicalizes
it (recording a symlink's target), records the resolved absolute path,
SHA-256 digest, and MCP `serverInfo.version` when available in local audit
metadata, and optionally enforces a deny-by-default digest allowlist via
`HARNESS_MCP_APPROVED_DIGESTS`. Provenance metadata is never sent to the model,
model events, or tool output. With
`external-wrapper`, the harness starts the configured absolute wrapper directly
with fixed arguments, then the wrapper starts the server; the wrapper is part of
the trusted deployment boundary and must be vetted accordingly. With
`bubblewrap`, the harness starts the configured absolute bubblewrap executable
with a fixed, harness-built argument list and the server as its child.

The harness calls `env_clear()` and supplies only the explicit, minimal
variables in `ProcessSpec::env`: a harness-controlled `HOME`, an explicit
`PATH` (or `HARNESS_MCP_PATH`), and no ambient environment. Proxy variables are
therefore not inherited, including `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
`NO_PROXY`, and their lowercase variants. A wrapper may alter the environment it
passes to the server; its behavior must be reviewed and enforced externally.

This environment sanitization does **not** prove that a third-party MCP binary
will never perform networking. In `none` and `external-wrapper` modes, strict
deployments must enforce the network boundary outside the Rust harness.
`bubblewrap` mode adds a Linux network namespace (`--unshare-net`) plus PID, IPC,
and UTS namespaces, clears the sandboxed environment, mounts selected system
paths read-only, and exposes only the controlled `HOME` and workspace (read-only
for `read_only`, writable for `workspace_mutation`). It is a defense-in-depth
control, not a substitute for a reviewed server and host policy.

### Frozen MCP configuration and lifecycle

This adapter is reference/optional work, not part of the active P0 execution
path. The TUI and headless preflight do not parse MCP settings, start MCP
servers, or require MCP executables, profiles, digests, or Bubblewrap. Static MCP
configuration and lifecycle wiring remain implemented for exactly three fixed
slots: `filesystem`, `git`, and `shell`. There is no server discovery, arbitrary
alias map, package download, install-on-demand, or model-selected
executable/argument/environment/cwd.

- `HARNESS_MCP_PROFILE=none` starts no servers and leaves tool capabilities
  unavailable. This is the default.
- `read_only` requires filesystem and Git servers and enables only
  `read_file`, `git_status`, and `git_diff`. A shell server is optional; when
  explicitly configured it enables argv-only `run_command`. Write and patch
  calls remain unavailable even if the filesystem server advertises them.
- `workspace_mutation` requires filesystem, Git, and shell servers and enables
  the fixed read/write/patch, read-only Git, and argv-only command routes.
- Git mutation is never wired by these profiles. The shell route carries a
  structured `program + args + cwd + timeout`, never a shell command string.
- `AgentTool::McpTool` remains denied by `Policy` after registration. Additional
  tools advertised outside the fixed manifest cause startup to fail closed.

Startup is all-or-nothing and occurs before terminal raw mode, task acceptance,
or any model/Jev request:

1. Validate the static profile, paths, arguments, sandbox, and bounds.
2. Build a `ProcessSpec` with `env_clear()` semantics, a harness-controlled
   `HOME`, an explicit `PATH`, the configured working directory, piped stdio,
   and `kill_on_drop(true)`.
3. Spawn the absolute executable directly without a shell.
4. Initialize MCP and send `notifications/initialized`.
5. List tools and reject missing/extra entries against the fixed manifest and
   active profile.
6. Install the client only after validation succeeds.

On any startup failure, every child already started by that attempt is
terminated and reaped. Shutdown is bounded and also terminates and reaps each
child. Startup stderr is bounded and passed through the same best-effort redaction
used for other diagnostics before it can appear in an error. Raw argument lists
are not logged.

`external-wrapper` is a static administrator/deployment control: a fixed,
absolute wrapper executable and fixed prefix arguments precede the fixed MCP
executable. The harness does not interpret a wrapper-specific security policy.
`bubblewrap` is a harness-managed Linux sandbox with a new network namespace and
a fixed mount/environment policy. A configured bubblewrap executable is validated
at startup; a missing or non-file executable fails configuration before any
server starts. `none` means no OS network sandbox at all; process environment
hygiene does not stop a child from opening sockets, so enabled `none` servers
require the explicit `HARNESS_MCP_ALLOW_UNSANDBOXED=true` opt-in.

`HARNESS_MCP_APPROVED_DIGESTS` is optional. When present it is a JSON object
mapping each enabled slot to a 64-character SHA-256 digest. The policy is
deny-by-default: an enabled server without an entry, or with a digest that does
not match the canonical executable, fails startup before any server is spawned.
An empty object denies every enabled server. The audit metadata records the
resolved executable path, digest, and reported version for local review; it is
not part of prompt context or model-facing events.

Only explicit MCP tests or the reference demo use this lifecycle. `--headless`
instead validates the active script runtime: canonical roots, interpreter, audit
path, action mappings, and Change availability. It does not open the TUI or run
a task.

Most end-to-end lifecycle tests use fake tool executors and fake model/Jev
adapters for orchestration. They prove the orchestration, policy, verification,
and persistence wiring. One boundary E2E test runs fixed MCP servers inside
harness-managed bubblewrap, performs a real workspace mutation through the mock
test server, verifies the resulting file, and checks redacted audit and Wiki
output. A separate deterministic demonstration runs read-only inspection,
approved mutation, and failed-verification recovery against temporary Cargo
fixtures, using the repository's mock MCP test binary with digest-pinned
Bubblewrap MCP slots, real Cargo verification, and bounded audit evidence. Runtime MCP tests also attempt an outbound loopback
connection from inside and outside the network namespace and inspect the sandbox
route table, network
interfaces, DNS resolution, and resolver configuration. Inside bubblewrap they
require the loopback connection and DNS resolution to fail, no default route,
and only the `lo` interface; the control process outside proves the same
listener is reachable. These tests use a test-only binary and temporary
directories; they still do not certify a third-party MCP server or a real model.
The CI `sandbox` lane installs Bubblewrap and sets `HARNESS_REQUIRE_BWRAP=1`, so
the positive containment and real-MCP tests fail rather than skip when the
sandbox is unavailable. The default lane prints an explicit sandbox-availability
report, and the unconditional missing-Bubblewrap fail-closed test always runs.

Accepted stability and demonstration scope:

```text
Run a small repeatable set of realistic repository tasks in the production-safe
local profile; collect bounded, redacted audit evidence; document operator setup
and recovery; and fix reliability issues found in real use. The demonstration
uses a scripted model for determinism and does not certify live model quality.
```

## Offline and vendored deployment

- Vendor dependencies on a controlled machine, review the vendored sources, and
  commit the intentional `.cargo/config.toml` source replacement with
  `Cargo.lock`.
- On the target, build and test with `cargo build --offline` and
  `cargo test --offline`. Do not rely on ambient registry access, automatic
  dependency updates, or an implicit online fallback.
- Pin and checksum-verify model files and reviewed helper scripts through the
  same offline supply-chain process as Cargo dependencies. Pin optional frozen
  MCP binaries only if that reference adapter is intentionally enabled.
- Offline Cargo is not a runtime network sandbox. It does not prevent the
  harness from contacting configured model/Jev endpoints, verifier commands from
  using the network, or helper scripts and optional MCP children from opening
  sockets.
- For a strict air gap, deny egress with the host firewall, network namespace,
  container policy, or equivalent OS control, and allow only the explicitly
  required loopback services.
- Before a real run, validate endpoint configuration, confirm the model and Jev
  services are bound only where intended, run the full test/Clippy matrix, verify
  the script root/interpreter and reviewed script digests, and start with a
  throwaway repository.

## Deterministic verification boundary

Verification commands are selected by harness code from repository signals and
executed through the fixed `run_verifier.py` script boundary. The model, Jev,
Wiki status, assistant finish reason, and tool results cannot add, remove, supply
a program/argument vector, or declare verification passed.

- Rust detection produces `program + args` values, never shell strings. The
  engine sends exactly one command per helper request; the helper validates the
  argv, cwd, timeout, and output cap, then spawns the command directly without a
  shell. Shell programs (`sh`, `bash`, `cmd`, PowerShell, and similar) and
  privilege/escalation wrappers such as `sudo`, `su`, `doas`, and `env` are
  rejected by both the Rust validation and helper boundary.
- Every command runs with an explicit working directory canonicalized beneath
  the repository root. Paths that escape the root are rejected before any
  command starts.
- Commands have a nonzero timeout and are killed on timeout or cancellation.
  The helper terminates the verifier process group on timeout; exact exit
  status, timeout state, and duration are recorded.
- Captured stdout and stderr are bounded, retaining a head and tail rather than
  unbounded process output. The failure tail is also bounded.
- Only a real executed command with exit code `0` satisfies a verification
  step. An empty command list is a blocked/not-passed report, never a success.
- Detection failures, execution errors, and empty profiles emit a failed
  `verification_finished` audit record. A completed Change task records the
  verifier step outcomes in its Wiki session trace.
- The verifier does not automatically install dependencies, create virtual
  environments, or run package-manager installation commands.
- Verification does not sandbox the child process or remove its network access.
  Strict deployments must apply filesystem and network isolation externally if
  verifier commands are not fully trusted.

## Recommended controls for strict air-gapped deployment

- Default-deny outbound network access with the host firewall or an egress
  policy.
- Allow only the specific loopback ports needed by the local model and routing
  services. Do not allow general loopback access to unrelated services.
- Run reviewed helper scripts from a fixed root outside the repository with an
  absolute interpreter. Do not use PATH lookup or model-selected scripts.
- If the frozen MCP adapter is intentionally enabled, run its servers in a
  network namespace, container, or bubblewrap sandbox with networking disabled.
  For the built-in Linux option, set the slot's `_SANDBOX_MODE=bubblewrap` and
  point `_SANDBOX_EXECUTABLE` at a vetted bubblewrap binary.
- Keep optional MCP binaries at fixed absolute paths, pin their SHA-256 digests
  with `HARNESS_MCP_APPROVED_DIGESTS`, and review the recorded
  path/version/digest audit metadata after upgrades.
- Use the local-first production profile in `README.md`: local LLM/Jev
  endpoints, reviewed script root/interpreter, no remote profile, and no
  `HARNESS_MCP_ALLOW_UNSANDBOXED`. Activate remote/work access only as a
  separate administrative decision.
- Mount the repository read-only where possible and keep helper scripts outside
  the project root.
- Do not inject proxy variables into the harness, helper scripts, or optional
  MCP children.
- Keep model and service binaries vetted, pinned, and updated only through an
  approved offline process.
- Review the `remote` feature and explicit endpoint configuration as a data
  egress decision before use.
