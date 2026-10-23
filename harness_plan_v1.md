# Local Agent Harness: Implementation Blueprint

*Single-agent, air-gapped Rust harness: MCP tool substrate, AI-Wiki memory, Jev (System 1) routing, local 30B (System 2) reasoning, ratatui terminal UI.*

## 0. Decisions and assumptions

- **One agent per task.** One orchestrator task owns one conversation. Learn, Plan, Verify and Review are *phases of that loop*, not separate agents.
- **Hard gates beat classifiers.** Jev's `Noul` may escalate or veto, but only a real exit code 0 from build/tests lets the loop finish. A classifier never overrides a failing build.
- **Fail open for routing, fail closed for completion.** If Jev is down or low-confidence, route to the 30B model. If verification cannot run, the task is `Blocked`, never silently `Done`.
- **Jev's wire contract was not specified.** Section 5 assumes three JSON endpoints (`/v1/choice`, `/v1/noul`, `/v1/score`) behind one adapter. Change only that adapter if your contract differs.
- **Air-gap.** `reqwest` with `default-features = false` (no TLS stack), plain HTTP to loopback only. Config validation rejects non-loopback hosts unless a cargo feature `remote` is enabled (which adds `rustls-tls` for the production work API).
- **MCP transport:** newline-delimited JSON-RPC 2.0 over stdio. Check the protocol revision string against your servers.
- Crate APIs (`gray_matter`, `ratatui`) drift between releases. Pin versions and adjust call sites if a compile error appears.

## 1. Architecture

```
            +------------- ratatui TUI (main task) --------------+
 keys ----->| select!{ crossterm EventStream, ui_rx, tick, worker }|
            +--------^---------------------------------+----------+
              UiEvent|  (mpsc)                          | Command (mpsc) + interrupt (watch)
            +--------+---------------------------------v----------+
            | Orchestrator: Learn > Plan > Act > Verify > Review   |
            |               > Compile > Done  (Blocked on failure) |
            +---+-----------+------------+---------------+--------+
                |           |            |               |
           Wiki (fs)   Jev (System 1)  MCP pool       LlmClient (System 2)
         .ai-wiki/*.md  choice/noul/   stdio JSON-RPC  llama-server
                        score                          /v1/chat/completions
```

**Cycle, per task:**

1. **Learn:** load `state-of-the-world.md`; Jev `score` reranks wiki sections against the task; inject the top sections.
2. **Plan:** one tool-less 30B turn produces a numbered plan, each step with a verification command.
3. **Act:** 30B turn with tools. Each tool call is parsed tolerantly, executed through MCP, and fed back bounded.
4. **Verify:** harness (not the model) runs build/tests. Failure goes back to Act, up to N retries.
5. **Review:** Jev `noul` judges completion against plan and evidence; a failing verdict triggers one self-review turn.
6. **Compile:** write-time wiki compilation, then exit.

## 2. Project layout and Cargo.toml

```
src/ main.rs  error.rs  config.rs  events.rs  llm.rs  jev.rs  tui.rs
     mcp/{mod,client,tools}.rs   wiki/{mod,compile}.rs   agent/{mod,verify}.rs
```

```toml
[package]
name = "local-agent-harness"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", default-features = false, features = ["json", "stream"] }
futures = "0.3"
thiserror = "1"
gray_matter = "0.2"
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }
tracing = "0.1"

[features]
remote = ["reqwest/rustls-tls"]
```

Bootstrap: `cargo new local-agent-harness --bin`, paste the above, then build modules in the order of Section 9.

### error.rs and config.rs

```rust
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
    #[error("http: {0}")] Http(#[from] reqwest::Error),
    #[error("mcp: {0}")] Mcp(String),
    #[error("jev: {0}")] Jev(String),
    #[error("llm: {0}")] Llm(String),
    #[error("wiki: {0}")] Wiki(String),
    #[error("blocked: {0}")] Blocked(String),
    #[error("cancelled")] Cancelled,
}

pub struct Config {
    pub root: std::path::PathBuf,
    pub llm_base: String,        // http://127.0.0.1:8080
    pub model: String,
    pub jev_base: String,        // http://127.0.0.1:9090
    pub min_route_conf: f32,     // e.g. 0.85
    pub max_act_turns: usize,    // e.g. 40
    pub max_verify_retries: u8,  // e.g. 3
    pub tool_output_cap: usize,  // bytes fed back per tool result, e.g. 4000
}

impl Config {
    pub fn validate(&self) -> Result<(), AgentError> {
        for u in [&self.llm_base, &self.jev_base] {
            let ok = u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost");
            if !ok && !cfg!(feature = "remote") {
                return Err(AgentError::Blocked(format!("non-loopback endpoint {u} in air-gapped build")));
            }
        }
        Ok(())
    }
}
```

## 3. Phase 1: Tool substrate (MCP)

### 3.1 JSON-RPC client over stdio (mcp/client.rs)

A reader task resolves pending requests by id through `oneshot` channels. Messages with an `id` but no `result`/`error` are server-initiated requests and are ignored (not matched to pending ids).

```rust
use std::{collections::HashMap, sync::{atomic::{AtomicU64, Ordering}, Arc}, time::Duration};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin, Command}, sync::{oneshot, Mutex}};
use crate::error::AgentError;

#[derive(Deserialize)] struct RpcError { code: i64, message: String }
#[derive(Deserialize)] struct RpcMsg { id: Option<u64>, result: Option<Value>, error: Option<RpcError> }
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<RpcMsg>>>>;

pub struct ToolOutcome { pub text: String, pub is_error: bool }
impl ToolOutcome {
    pub fn err(s: impl Into<String>) -> Self { Self { text: s.into(), is_error: true } }
}

pub struct McpClient {
    pub name: String,
    _child: Child,                 // kill_on_drop: no orphaned servers
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
}

impl McpClient {
    pub async fn spawn(name: &str, cmd: &str, args: &[&str]) -> Result<Self, AgentError> {
        use std::process::Stdio;
        let mut child = Command::new(cmd).args(args)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true).spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| AgentError::Mcp("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| AgentError::Mcp("no stdout".into()))?;
        let pending: Pending = Arc::default();
        let p = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<RpcMsg>(&line) else { continue };
                if msg.result.is_none() && msg.error.is_none() { continue; }
                if let Some(id) = msg.id {
                    if let Some(tx) = p.lock().await.remove(&id) { let _ = tx.send(msg); }
                }
            }
            p.lock().await.clear(); // dropped senders wake waiters with an error
        });
        let c = Self { name: name.into(), _child: child, stdin: Mutex::new(stdin), pending, next_id: AtomicU64::new(1) };
        c.request("initialize", json!({
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "local-agent-harness", "version": "0.1.0" }
        })).await?;
        c.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await?;
        Ok(c)
    }

    async fn send(&self, v: Value) -> Result<(), AgentError> {
        let mut line = serde_json::to_string(&v)?;
        line.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(line.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AgentError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let msg = tokio::time::timeout(Duration::from_secs(120), rx).await
            .map_err(|_| AgentError::Mcp(format!("{method} timed out")))?
            .map_err(|_| AgentError::Mcp("server closed".into()))?;
        match msg.error {
            Some(e) => Err(AgentError::Mcp(format!("{} ({})", e.message, e.code))),
            None => Ok(msg.result.unwrap_or(Value::Null)),
        }
    }

    /// Transport failures become `is_error` outcomes the model can read and correct.
    pub async fn call_tool(&self, tool: &str, args: Value) -> ToolOutcome {
        match self.request("tools/call", json!({ "name": tool, "arguments": args })).await {
            Ok(v) => {
                let text = v["content"].as_array()
                    .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                ToolOutcome { text, is_error: v["isError"].as_bool().unwrap_or(false) }
            }
            Err(e) => ToolOutcome::err(e.to_string()),
        }
    }
}

pub struct McpPool(pub HashMap<String, McpClient>);
impl McpPool {
    pub async fn dispatch(&self, t: &crate::mcp::tools::AgentTool) -> ToolOutcome {
        let (server, tool, args) = t.route();
        match self.0.get(server) {
            Some(c) => c.call_tool(tool, args).await,
            None => ToolOutcome::err(format!("server '{server}' not running")),
        }
    }
}
```

Start servers in `main` (names are examples; use your vetted binaries): `McpClient::spawn("fs", "mcp-server-filesystem", &[root])`, `"git"` likewise, and `"shell"` for a small allowlisted command server you control.

### 3.2 Strong tool types with tolerant parsing (mcp/tools.rs)

Local models vary case, separators, and argument names. Normalize before matching, and return a *teaching* error instead of failing.

```rust
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum AgentTool {
    RunCommand { command: String, cwd: Option<String>, timeout_secs: u64 },
    WriteFile { path: String, content: String },
    ReadFile { path: String },
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}
fn arg<'a>(a: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| a.get(*k).and_then(Value::as_str))
}

impl AgentTool {
    pub fn parse(name: &str, a: &Value) -> Result<Self, String> {
        match norm(name).as_str() {
            "runcommand" | "bash" | "shell" | "exec" => Ok(Self::RunCommand {
                command: arg(a, &["command", "cmd"]).ok_or("run_command needs string `command`")?.into(),
                cwd: arg(a, &["cwd", "dir"]).map(Into::into),
                timeout_secs: a.get("timeout_secs").and_then(Value::as_u64).unwrap_or(120).min(900),
            }),
            "writefile" | "write" | "createfile" => Ok(Self::WriteFile {
                path: arg(a, &["path", "file_path", "filename"]).ok_or("write_file needs string `path`")?.into(),
                content: arg(a, &["content", "text", "contents"]).ok_or("write_file needs string `content`")?.into(),
            }),
            "readfile" | "read" | "cat" => Ok(Self::ReadFile {
                path: arg(a, &["path", "file_path", "filename"]).ok_or("read_file needs string `path`")?.into(),
            }),
            other => Err(format!("unknown tool '{other}'")),
        }
    }

    /// (mcp server, mcp tool name, arguments)
    pub fn route(&self) -> (&'static str, &'static str, Value) {
        match self {
            Self::RunCommand { command, cwd, timeout_secs } =>
                ("shell", "run", json!({ "command": command, "cwd": cwd, "timeout_secs": timeout_secs })),
            Self::WriteFile { path, content } => ("fs", "write_file", json!({ "path": path, "content": content })),
            Self::ReadFile { path } => ("fs", "read_file", json!({ "path": path })),
        }
    }
}

/// OpenAI-style tool specs sent to the model (keep descriptions terse: tokens are budget).
pub fn tool_specs() -> Value {
    let f = |n: &str, d: &str, props: Value, req: Value| json!({ "type": "function",
        "function": { "name": n, "description": d, "parameters": { "type": "object", "properties": props, "required": req } } });
    json!([
        f("run_command", "Run a shell command in the project.", json!({ "command": { "type": "string" } }), json!(["command"])),
        f("read_file", "Read a text file.", json!({ "path": { "type": "string" } }), json!(["path"])),
        f("write_file", "Create or overwrite a file.", json!({ "path": { "type": "string" }, "content": { "type": "string" } }), json!(["path", "content"]))
    ])
}
```

## 4. Phase 2: Memory layer (Karpathy-style AI Wiki)

**Layout:** `.ai-wiki/state-of-the-world.md` (the one file injected at session start), `.ai-wiki/log.md` (append-only dated one-liners, never injected).

```markdown
---
title: State of the world
tags: [rust, cli]
updated: 2026-10-22
session_count: 3
status: green
---
## Current state
...
## Decisions
...
## Open items
...
## How to verify
`cargo check --all-targets && cargo test`
```

### 4.1 Session-start ingestion (wiki/mod.rs)

```rust
use gray_matter::{engine::YAML, Matter};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Frontmatter {
    #[serde(default)] pub title: String,
    #[serde(default)] pub tags: Vec<String>,
    #[serde(default)] pub updated: String,
    #[serde(default)] pub session_count: u32,
    #[serde(default)] pub status: String,
}
pub struct WikiContext { pub front: Frontmatter, pub body: String }

pub async fn load(root: &Path) -> Result<WikiContext, std::io::Error> {
    let path = root.join(".ai-wiki/state-of-the-world.md");
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound =>
            return Ok(WikiContext { front: Frontmatter::default(), body: String::new() }),
        Err(e) => return Err(e),
    };
    let parsed = Matter::<YAML>::new().parse(&raw);
    let front = parsed.data.and_then(|d| d.deserialize::<Frontmatter>().ok()).unwrap_or_default();
    Ok(WikiContext { front, body: parsed.content })
}

/// Split on level-2 headings so Jev `score` can rerank sections against the task.
pub fn sections(body: &str) -> Vec<String> {
    body.split("\n## ").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}
```

### 4.2 Write-time compilation (wiki/compile.rs)

Two layers. A **deterministic digest** (files written, commands and exit codes, verification result, `git diff --stat`) cannot hallucinate. A **single 30B call** merges the digest with the prior page into the new body, with a strict output contract and a size cap. The write is atomic (temp file, then rename) so a crash never leaves a half-written wiki.

```rust
pub struct SessionTrace {
    pub files_written: std::collections::BTreeSet<String>,
    pub commands: Vec<(String, i32)>,
    pub verify: Option<crate::agent::verify::VerifyReport>,
    pub diff_stat: String,
    pub plan: String,
}
impl SessionTrace {
    pub fn digest(&self) -> String {
        let cmds: Vec<String> = self.commands.iter().rev().take(15).map(|(c, x)| format!("- `{c}` -> {x}")).collect();
        format!("Files written: {:?}\nCommands:\n{}\nVerify: {}\nDiff:\n{}\nPlan:\n{}",
            self.files_written, cmds.join("\n"),
            self.verify.as_ref().map_or("not run".into(), |v| if v.passed { "PASSED".into() } else { "FAILED".into() }),
            self.diff_stat, self.plan)
    }
}

pub async fn compile(root: &std::path::Path, prior: &WikiContext, trace: &SessionTrace,
                     llm: &crate::llm::LlmClient, today: &str) -> Result<(), crate::error::AgentError> {
    let sys = "You maintain a project state file. Output ONLY the new markdown body (no frontmatter), \
               at most 80 lines, with sections: ## Current state, ## Decisions, ## Open items, ## How to verify. \
               Keep prior facts that are still true; delete stale ones; never invent results.";
    let user = format!("PRIOR:\n{}\n\nSESSION DIGEST:\n{}", prior.body, trace.digest());
    let body = llm.complete_text(sys, &user).await?;
    let status = match &trace.verify { Some(v) if v.passed => "green", _ => "red" };
    let page = format!("---\ntitle: State of the world\nupdated: {today}\nsession_count: {}\nstatus: {status}\n---\n{}\n",
        prior.front.session_count + 1, body.trim());
    let dir = root.join(".ai-wiki");
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = dir.join(".state.tmp");
    tokio::fs::write(&tmp, page).await?;
    tokio::fs::rename(&tmp, dir.join("state-of-the-world.md")).await?;
    use tokio::io::AsyncWriteExt;
    let mut log = tokio::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.md")).await?;
    log.write_all(format!("- {today}: {status}, {} files written\n", trace.files_written.len()).as_bytes()).await?;
    Ok(())
}
```

## 5. Phase 3: Routing brain (Jev, System 1)

**Contract assumed** (adapt here only): `POST /v1/choice {prompt, options[]} -> {index, confidence}`; `POST /v1/noul {claim, evidence} -> {pass, confidence}`; `POST /v1/score {query, docs[]} -> {scores[]}`.

Raw wire structs are validated into enums *before* anything else in the program sees them, so an out-of-range index or NaN score can never reach the 30B model or the state machine.

```rust
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use crate::error::AgentError;

#[derive(Deserialize)] struct RawChoice { index: i64, confidence: f32 }
#[derive(Deserialize)] struct RawNoul { pass: bool, confidence: f32 }
#[derive(Deserialize)] struct RawScore { scores: Vec<f32> }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Route { Pick(usize), Escalate }
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict { Pass(f32), Fail(f32), Uncertain }

pub struct JevClient { http: reqwest::Client, base: String }

impl JevClient {
    pub fn new(base: &str) -> Result<Self, AgentError> {
        let http = reqwest::Client::builder().timeout(Duration::from_millis(800)).build()?;
        Ok(Self { http, base: base.trim_end_matches('/').into() })
    }
    async fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T, AgentError> {
        Ok(self.http.post(format!("{}{}", self.base, path)).json(&body).send().await?
            .error_for_status()?.json::<T>().await?)
    }

    pub async fn choice(&self, prompt: &str, options: &[&str], min_conf: f32) -> Route {
        match self.post::<RawChoice>("/v1/choice", json!({ "prompt": prompt, "options": options })).await {
            Ok(r) => match usize::try_from(r.index) {
                Ok(i) if i < options.len() && r.confidence.is_finite() && r.confidence >= min_conf => Route::Pick(i),
                _ => Route::Escalate,
            },
            Err(_) => Route::Escalate,           // fail open to System 2
        }
    }

    pub async fn noul(&self, claim: &str, evidence: &str, threshold: f32) -> Verdict {
        match self.post::<RawNoul>("/v1/noul", json!({ "claim": claim, "evidence": evidence })).await {
            Ok(r) if r.confidence.is_finite() && r.confidence >= threshold =>
                if r.pass { Verdict::Pass(r.confidence) } else { Verdict::Fail(r.confidence) },
            _ => Verdict::Uncertain,
        }
    }

    pub async fn score(&self, query: &str, docs: &[String]) -> Option<Vec<f32>> {
        let r: RawScore = self.post("/v1/score", json!({ "query": query, "docs": docs })).await.ok()?;
        (r.scores.len() == docs.len() && r.scores.iter().all(|s| s.is_finite())).then_some(r.scores)
    }
}
```

**Where System 1 is actually used:** (a) wiki-section reranking in Learn, (b) deciding whether a model turn that emitted no tool call is a completion claim or a stall (`choice` over `["claims_done","asks_user","stalled"]`), (c) the `noul` completion check in Review. Everything else goes to the 30B model; do not invent more routing points until traces show a need.

*Part 2 (Section 6 onward: LLM client, events, verification gate, orchestrator loop, TUI, Sonnet 5.5 notes, build order) continues below.*

## 6. Phase 4: Orchestration, LLM client, TUI

### 6.1 Local LLM client (llm.rs)

Start the server with `llama-server --jinja -m model.gguf -c 32768 --port 8080` so the chat template handles `tools`. The client streams SSE, forwards tokens to the UI, and reassembles fragmented tool-call deltas by `index`.

```rust
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use crate::{error::AgentError, events::UiEvent};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FnCall { pub name: String, pub arguments: String }
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ToolCall { pub id: String, #[serde(rename = "type")] pub kind: String, pub function: FnCall }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub tool_call_id: Option<String>,
}
impl ChatMessage {
    pub fn new(role: &str, text: impl Into<String>) -> Self {
        Self { role: role.into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn tool(id: String, text: String) -> Self {
        Self { role: "tool".into(), content: Some(text), tool_calls: None, tool_call_id: Some(id) }
    }
}

#[derive(Default, Debug)]
pub struct AssistantTurn { pub text: String, pub calls: Vec<ToolCall>, pub finish: String }
impl AssistantTurn {
    pub fn as_message(&self) -> ChatMessage {
        ChatMessage { role: "assistant".into(),
            content: (!self.text.is_empty()).then(|| self.text.clone()),
            tool_calls: (!self.calls.is_empty()).then(|| self.calls.clone()), tool_call_id: None }
    }
    fn finish_ids(mut self) -> Self {      // some servers omit ids: synthesize stable ones
        for (i, c) in self.calls.iter_mut().enumerate() { if c.id.is_empty() { c.id = format!("call_{i}"); } }
        self
    }
}

pub struct LlmClient { http: reqwest::Client, base: String, pub model: String, pub max_tokens: u32, pub temperature: f32 }

impl LlmClient {
    pub async fn stream_chat(&self, msgs: &[ChatMessage], tools: &Value,
                             ui: &UnboundedSender<UiEvent>) -> Result<AssistantTurn, AgentError> {
        let mut body = json!({ "model": self.model, "messages": msgs, "stream": true,
            "temperature": self.temperature, "max_tokens": self.max_tokens });
        if tools.as_array().map_or(false, |a| !a.is_empty()) { body["tools"] = tools.clone(); }
        let resp = self.http.post(format!("{}/v1/chat/completions", self.base))
            .json(&body).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();
        let (mut buf, mut turn) = (Vec::<u8>::new(), AssistantTurn::default());
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=pos).collect();     // byte buffer: safe across UTF-8 splits
                let line = String::from_utf8_lossy(&raw);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" { return Ok(turn.finish_ids()); }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                let (choice, delta) = (&v["choices"][0], &v["choices"][0]["delta"]);
                if let Some(t) = delta["content"].as_str() {
                    turn.text.push_str(t);
                    let _ = ui.send(UiEvent::Token(t.to_string()));
                }
                for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                    let i = tc["index"].as_u64().unwrap_or(0) as usize;
                    while turn.calls.len() <= i { turn.calls.push(ToolCall { kind: "function".into(), ..Default::default() }); }
                    let c = &mut turn.calls[i];
                    if let Some(id) = tc["id"].as_str() { c.id = id.into(); }
                    if let Some(n) = tc["function"]["name"].as_str() { c.function.name.push_str(n); }
                    if let Some(a) = tc["function"]["arguments"].as_str() { c.function.arguments.push_str(a); }
                }
                if let Some(f) = choice["finish_reason"].as_str() { turn.finish = f.into(); }
            }
        }
        Ok(turn.finish_ids())
    }
}
```

`complete_text(system, user)` is a thin non-streaming variant (same endpoint, `stream: false`, return `choices[0].message.content`). Add a `trait LlmBackend` with this client and an Anthropic Messages adapter for the production work API; the orchestrator should depend only on the trait.

### 6.2 Events and state (events.rs)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Learn, Plan, Act, Verify, Review, Compile, Done, Blocked }

/// worker -> UI
pub enum UiEvent {
    Token(String),
    PhaseChanged(Phase),
    ToolStarted { id: String, label: String },
    ToolFinished { id: String, ok: bool, preview: String },
    NeedApproval { label: String },
    Verify { passed: bool, summary: String },
    Notice(String),
    Finished(Result<(), String>),
}
/// UI -> worker (Esc uses a separate `watch` counter so a turn can be aborted mid-stream)
pub enum Command { Submit(String), Approve(bool), Quit }
```

### 6.3 Verification gate (agent/verify.rs)

The harness, not the model, chooses and runs the checks. Dependencies are installed with package managers into project-local locations; `sudo` is rejected everywhere.

```rust
use std::{path::Path, time::Duration};

pub struct VerifyReport { pub passed: bool, pub steps: Vec<(String, i32)>, pub tail: String }

pub fn detect(root: &Path) -> Vec<String> {
    let mut v = vec![];
    let has = |f: &str| root.join(f).exists();
    if has("Cargo.toml") { v.push("cargo check --all-targets".into()); v.push("cargo test".into()); }
    if has("package.json") {
        if !has("node_modules") { v.push("npm install".into()); }
        v.push("npm test --if-present".into());
    }
    if has("requirements.txt") {
        if !has(".venv") { v.push("python3 -m venv .venv".into()); }
        v.push(".venv/bin/pip install -r requirements.txt".into());
    }
    if has("pytest.ini") || has("pyproject.toml") || has("requirements.txt") {
        v.push(if has(".venv") { ".venv/bin/python -m pytest -q" } else { "python3 -m pytest -q" }.into());
    }
    v
}

pub async fn run(root: &Path, cmds: &[String], cap: usize) -> VerifyReport {
    let mut steps = vec![];
    let mut tail = String::new();
    let mut passed = !cmds.is_empty();         // nothing to run is NOT a pass
    for c in cmds {
        if c.contains("sudo") { steps.push((c.clone(), -1)); passed = false; tail = "sudo is forbidden".into(); break; }
        let fut = tokio::process::Command::new("sh").arg("-c").arg(c).current_dir(root).kill_on_drop(true).output();
        let (code, out) = match tokio::time::timeout(Duration::from_secs(900), fut).await {
            Ok(Ok(o)) => (o.status.code().unwrap_or(-1),
                format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Ok(Err(e)) => (-1, e.to_string()),
            Err(_) => (-2, "timed out".into()),
        };
        steps.push((c.clone(), code));
        if code != 0 { passed = false; tail = crate::agent::bound(&out, cap); break; }
    }
    VerifyReport { passed, steps, tail }
}
```

If `detect` returns nothing and the plan names no verification command, the orchestrator goes to `Blocked` with a message asking the user for one. It never reports done on unverified code.

### 6.4 The orchestrator and its select loop (agent/mod.rs)

```rust
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use crate::{error::AgentError, events::*, llm::*, jev::*, mcp::{client::*, tools::*}, wiki};

pub mod verify;

/// Head+tail truncation keeps both the command and the error in context without wasting tokens.
pub fn bound(s: &str, cap: usize) -> String {
    if s.len() <= cap { return s.to_string(); }
    let h = cap / 2;
    let (a, b) = (s.floor_char_boundary(h), s.ceil_char_boundary(s.len() - h));
    format!("{}\n...[{} bytes omitted]...\n{}", &s[..a], b - a, &s[b..])
}

pub struct Orchestrator {
    pub cfg: crate::config::Config,
    pub llm: LlmClient, pub jev: JevClient, pub mcp: McpPool,
    pub ui: mpsc::UnboundedSender<UiEvent>,
    pub cmd_rx: mpsc::Receiver<Command>,
    pub interrupt: watch::Receiver<u64>,
    msgs: Vec<ChatMessage>, tools: Value,
    trace: wiki::compile::SessionTrace,
    prior: Option<wiki::WikiContext>,
    task: String, verified: bool, verify_retries: u8, review_rounds: u8,
}

impl Orchestrator {
    pub async fn run(mut self, cancel: CancellationToken) -> Result<(), AgentError> {
        // wait for the first task from the UI
        self.task = loop {
            match self.cmd_rx.recv().await {
                Some(Command::Submit(t)) => break t,
                Some(Command::Quit) | None => return Ok(()),
                _ => {}
            }
        };
        let mut phase = Phase::Learn;
        loop {
            let _ = self.ui.send(UiEvent::PhaseChanged(phase));
            phase = match phase {
                Phase::Learn   => self.learn().await?,
                Phase::Plan    => self.plan(&cancel).await?,
                Phase::Act     => self.act(&cancel).await?,
                Phase::Verify  => self.verify().await?,
                Phase::Review  => self.review(&cancel).await?,
                Phase::Compile => { self.compile().await?; Phase::Done }
                Phase::Done    => return Ok(()),
                Phase::Blocked => return Err(AgentError::Blocked("needs user input or a verifier".into())),
            };
        }
    }

    // ---- Learn: wiki ingestion + System 1 reranking ----
    async fn learn(&mut self) -> Result<Phase, AgentError> {
        let w = wiki::load(&self.cfg.root).await?;
        let secs = wiki::sections(&w.body);
        let ranked: Vec<String> = match self.jev.score(&self.task, &secs).await {
            Some(sc) => {
                let mut idx: Vec<usize> = (0..secs.len()).collect();
                idx.sort_by(|a, b| sc[*b].total_cmp(&sc[*a]));
                idx.into_iter().take(4).map(|i| secs[i].clone()).collect()
            }
            None => secs.into_iter().take(4).collect(),    // Jev down: first sections
        };
        let system = format!(
            "<role>You are the sole executing agent for this repository task.</role>\n\
             <wiki status=\"{}\">\n{}\n</wiki>\n\
             <rules>Use tools to act. Never claim completion without a passing verification. \
             No sudo; install dependencies with the project's package manager. Be concise.</rules>",
            w.front.status, ranked.join("\n\n"));
        self.msgs = vec![ChatMessage::new("system", system), ChatMessage::new("user", self.task.clone())];
        self.prior = Some(w);
        Ok(Phase::Plan)
    }

    // ---- Plan: one tool-less turn ----
    async fn plan(&mut self, cancel: &CancellationToken) -> Result<Phase, AgentError> {
        self.msgs.push(ChatMessage::new("user",
            "Write a numbered plan (max 8 steps). Each step must name how it will be verified."));
        let turn = self.turn(cancel, &Value::Null).await?;
        self.trace.plan = turn.text.clone();
        self.msgs.push(turn.as_message());
        Ok(Phase::Act)
    }

    /// One model turn, abortable by cancel token or the Esc interrupt counter.
    async fn turn(&mut self, cancel: &CancellationToken, tools: &Value) -> Result<AssistantTurn, AgentError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AgentError::Cancelled),
            _ = self.interrupt.changed() => Err(AgentError::Blocked("interrupted by user".into())),
            r = self.llm.stream_chat(&self.msgs, tools, &self.ui) => r,
        }
    }

    // ---- Act: model turn -> tool switch -> feed back ----
    async fn act(&mut self, cancel: &CancellationToken) -> Result<Phase, AgentError> {
        for _ in 0..self.cfg.max_act_turns {
            let tools = self.tools.clone();
            let turn = self.turn(cancel, &tools).await?;
            self.msgs.push(turn.as_message());
            if turn.calls.is_empty() {
                // System 1: is this a completion claim, a question, or a stall?
                let r = self.jev.choice(&turn.text, &["claims_done", "asks_user", "stalled"], self.cfg.min_route_conf).await;
                match r {
                    Route::Pick(1) => return Ok(Phase::Blocked),
                    Route::Pick(2) => {
                        self.msgs.push(ChatMessage::new("user", "Continue: call a tool or state what is blocking you."));
                        continue;
                    }
                    _ => return Ok(Phase::Verify),   // claims_done or Escalate: let the harness check
                }
            }
            for call in turn.calls {
                let parsed = serde_json::from_str::<Value>(&call.function.arguments)
                    .map_err(|e| format!("arguments are not valid JSON: {e}"))
                    .and_then(|a| AgentTool::parse(&call.function.name, &a));
                let outcome = match parsed {
                    Ok(tool) => {
                        let _ = self.ui.send(UiEvent::ToolStarted { id: call.id.clone(), label: format!("{tool:?}") });
                        self.exec(tool).await
                    }
                    Err(msg) => ToolOutcome::err(format!("{msg}. Valid tools: run_command, read_file, write_file.")),
                };
                let _ = self.ui.send(UiEvent::ToolFinished { id: call.id.clone(), ok: !outcome.is_error, preview: bound(&outcome.text, 200) });
                let body = bound(&outcome.text, self.cfg.tool_output_cap);
                let body = if outcome.is_error { format!("ERROR: {body}") } else { body };
                self.msgs.push(ChatMessage::tool(call.id, body));
            }
        }
        Ok(Phase::Blocked)    // turn budget exhausted
    }

    // ---- Tool execution switch ----
    async fn exec(&mut self, tool: AgentTool) -> ToolOutcome {
        match &tool {
            AgentTool::ReadFile { path } => {
                if let Err(e) = self.in_root(path) { return ToolOutcome::err(e); }
                self.mcp.dispatch(&tool).await
            }
            AgentTool::WriteFile { path, .. } => {
                if let Err(e) = self.in_root(path) { return ToolOutcome::err(e); }
                let o = self.mcp.dispatch(&tool).await;
                if !o.is_error { self.trace.files_written.insert(path.clone()); self.verified = false; }
                o
            }
            AgentTool::RunCommand { command, .. } => {
                if command.contains("sudo") { return ToolOutcome::err("sudo is not allowed; use npm/pip/cargo locally"); }
                if Self::risky(command) && !self.approve(command).await { return ToolOutcome::err("denied by user"); }
                let o = self.mcp.dispatch(&tool).await;
                self.trace.commands.push((command.clone(), if o.is_error { 1 } else { 0 }));
                o
            }
        }
    }

    fn in_root(&self, p: &str) -> Result<(), String> {
        let abs = self.cfg.root.join(p);
        let norm = abs.components().fold(std::path::PathBuf::new(), |mut a, c| {
            if c == std::path::Component::ParentDir { a.pop(); } else { a.push(c); } a });
        if norm.starts_with(&self.cfg.root) { Ok(()) } else { Err(format!("path '{p}' escapes project root")) }
    }
    fn risky(c: &str) -> bool { ["rm -rf", "git push", "git reset --hard", "curl ", "wget ", "chmod -R"].iter().any(|p| c.contains(p)) }
    async fn approve(&mut self, label: &str) -> bool {
        let _ = self.ui.send(UiEvent::NeedApproval { label: label.into() });
        loop { match self.cmd_rx.recv().await { Some(Command::Approve(b)) => return b, Some(Command::Quit) | None => return false, _ => {} } }
    }

    // ---- Verify: harness-run, model cannot skip ----
    async fn verify(&mut self) -> Result<Phase, AgentError> {
        let cmds = verify::detect(&self.cfg.root);
        if cmds.is_empty() { return Ok(Phase::Blocked); }
        let rep = verify::run(&self.cfg.root, &cmds, self.cfg.tool_output_cap).await;
        let _ = self.ui.send(UiEvent::Verify { passed: rep.passed, summary: format!("{:?}", rep.steps) });
        if !rep.passed {
            self.verify_retries += 1;
            let out_of_retries = self.verify_retries > self.cfg.max_verify_retries;
            if !out_of_retries {
                self.msgs.push(ChatMessage::new("user", format!("Verification failed:\n{}\nFix the cause, do not weaken the checks.", rep.tail)));
            }
            self.trace.verify = Some(rep);
            return Ok(if out_of_retries { Phase::Blocked } else { Phase::Act });
        }
        self.verified = true;
        self.trace.verify = Some(rep);
        Ok(Phase::Review)
    }

    // ---- Review: System 1 verdict, then at most one System 2 self-review ----
    async fn review(&mut self, cancel: &CancellationToken) -> Result<Phase, AgentError> {
        if !self.trace.files_written.is_empty() && !self.verified { return Ok(Phase::Verify); }   // hard gate
        let evidence = format!("{}\n{}", self.trace.plan, self.trace.digest());
        match self.jev.noul("All plan steps are implemented and verified.", &evidence, 0.8).await {
            Verdict::Pass(_) => return Ok(Phase::Compile),
            _ if self.review_rounds >= 1 => return Ok(Phase::Compile),   // bounded; wiki records status
            _ => {}
        }
        self.review_rounds += 1;
        self.msgs.push(ChatMessage::new("user", "List any unmet plan steps as a tool call to fix them, or reply exactly DONE."));
        let tools = self.tools.clone();
        let turn = self.turn(cancel, &tools).await?;
        let done = turn.calls.is_empty() && turn.text.trim() == "DONE";
        self.msgs.push(turn.as_message());
        Ok(if done { Phase::Compile } else { Phase::Act })
    }

    async fn compile(&mut self) -> Result<(), AgentError> {
        let prior = self.prior.take().unwrap_or(wiki::WikiContext { front: Default::default(), body: String::new() });
        wiki::compile::compile(&self.cfg.root, &prior, &self.trace, &self.llm, "today").await
    }
}
```

Notes: `floor_char_boundary` and `ceil_char_boundary` are recent std additions; if your toolchain lacks them, scan to the nearest `is_char_boundary`. Replace the `"today"` placeholder with a real date string passed in from `main`. `Orchestrator::build` (not shown) constructs the clients, spawns the MCP servers, and sets `tools = tools::tool_specs()`.

### 6.5 TUI event loop (main.rs and tui.rs)

Rendering never touches the network. The UI task only multiplexes keyboard input, worker events, and a tick; the worker runs as its own task.

```rust
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), error::AgentError> {
    let cfg = config::Config::from_env()?;      // implement: env vars or a TOML file
    cfg.validate()?;
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<events::UiEvent>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<events::Command>(32);
    let (int_tx, int_rx) = watch::channel(0u64);
    let cancel = CancellationToken::new();

    let orch = agent::Orchestrator::build(cfg, ui_tx.clone(), cmd_rx, int_rx).await?;   // spawns MCP servers
    let mut worker = tokio::spawn(orch.run(cancel.clone()));
    let mut done = false;

    let mut term = ratatui::init();             // installs a panic hook that restores the terminal
    let mut app = tui::AppState::default();
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    loop {
        term.draw(|f| tui::draw(f, &app))?;
        tokio::select! {
            _ = tick.tick() => {}
            Some(ev) = ui_rx.recv() => app.apply(ev),
            Some(Ok(Event::Key(k))) = keys.next() => {
                if k.kind != KeyEventKind::Press { continue; }
                match (k.modifiers, k.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) => { cancel.cancel(); break; }
                    (_, KeyCode::Esc) => { int_tx.send_modify(|n| *n += 1); }
                    (_, KeyCode::Enter) => {
                        let t = app.take_input();
                        let cmd = if app.awaiting_approval.take().is_some() { events::Command::Approve(t.trim() == "y") }
                                  else { events::Command::Submit(t) };
                        let _ = cmd_tx.send(cmd).await;
                    }
                    (_, KeyCode::Char(c)) => app.input.push(c),
                    (_, KeyCode::Backspace) => { app.input.pop(); }
                    _ => {}
                }
            }
            res = &mut worker, if !done => {
                done = true;
                let r = match res { Ok(Ok(())) => Ok(()), Ok(Err(e)) => Err(e.to_string()), Err(e) => Err(e.to_string()) };
                app.apply(events::UiEvent::Finished(r));
            }
        }
    }
    ratatui::restore();
    Ok(())
}
```

```rust
// tui.rs
use ratatui::{layout::{Constraint, Layout}, widgets::{Block, Paragraph, Wrap}, Frame};
use crate::events::{Phase, UiEvent};

pub struct AppState { pub phase: Phase, pub transcript: String, pub input: String, pub tool_count: u32, pub awaiting_approval: Option<String> }
impl Default for AppState {
    fn default() -> Self { Self { phase: Phase::Learn, transcript: String::new(), input: String::new(), tool_count: 0, awaiting_approval: None } }
}
impl AppState {
    pub fn take_input(&mut self) -> String { std::mem::take(&mut self.input) }
    pub fn apply(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Token(t) => self.transcript.push_str(&t),
            UiEvent::PhaseChanged(p) => { self.phase = p; self.transcript.push_str(&format!("\n[{p:?}]\n")); }
            UiEvent::ToolStarted { label, .. } => { self.tool_count += 1; self.transcript.push_str(&format!("\n> {label}\n")); }
            UiEvent::ToolFinished { ok, preview, .. } => self.transcript.push_str(&format!("{} {preview}\n", if ok { "ok" } else { "ERR" })),
            UiEvent::NeedApproval { label } => { self.transcript.push_str(&format!("\nApprove `{label}`? type y/n + Enter\n")); self.awaiting_approval = Some(label); }
            UiEvent::Verify { passed, summary } => self.transcript.push_str(&format!("\nverify: {} {summary}\n", if passed { "PASS" } else { "FAIL" })),
            UiEvent::Notice(n) => self.transcript.push_str(&format!("\n{n}\n")),
            UiEvent::Finished(r) => self.transcript.push_str(&format!("\nfinished: {r:?}\n")),
        }
    }
}

pub fn draw(f: &mut Frame, app: &AppState) {
    let [head, body, input] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)]).areas(f.area());
    f.render_widget(Paragraph::new(format!(" phase: {:?} | tool calls: {} | Esc: interrupt, Ctrl-C: quit", app.phase, app.tool_count)), head);
    let scroll = app.transcript.lines().count().saturating_sub(body.height as usize) as u16;
    f.render_widget(Paragraph::new(app.transcript.as_str()).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(Block::bordered()), body);
    f.render_widget(Paragraph::new(app.input.as_str()).block(Block::bordered().title("task / reply")), input);
}
```

## 7. Adapting prompts and settings for Claude Sonnet 5.5 (production API path)

The local path and the production path share one orchestrator; only the `LlmBackend` implementation changes.

- **Backend adapter.** Implement the Anthropic Messages API as a second `LlmBackend`. Map the internal `ChatMessage` list to content blocks: assistant `tool_use` blocks, followed by a user message containing the matching `tool_result` blocks (with `is_error: true` on failures). Keep all tool results for one assistant turn in the same user message so block ordering stays valid.
- **Effort and budget.** Expose `effort` (`medium` default, `high` for Plan and Review) and a generous `max_tokens` in `Config` so reasoning plus output is not truncated. Confirm exact parameter names in Anthropic's current docs; this blueprint does not hard-code them.
- **Prompt shape.** Keep the XML-sectioned system prompt from `learn()` (`<role>`, `<wiki>`, `<rules>`). State outcomes and stopping conditions, not procedures: stop only when blocked or when verification passes, and do not add features or tests beyond the task.
- **Initiative.** The loop already supplies persistence (Act continues until the model stops calling tools). The `risky()` gate and the `Blocked` state provide pause-before-risky-action behavior in code rather than relying on prompting.
- **Progress visibility.** Emit `Notice` events at phase boundaries. Harness-originated reminders (verification failures, nudges) are injected as clearly labeled user messages, never mixed into assistant content.
- **Local-model tuning the 30B path needs and the API path does not:** lower temperature (0.1 to 0.3) for tool turns, shorter tool descriptions, and the tolerant parser from Section 3.2.

## 8. Security and token-economy checklist

- **Process isolation:** run the `shell` MCP server under bubblewrap/firejail or a container with no network; cwd jailed to the repo.
- **Path control:** `in_root` is a lexical check. Also reject symlinks that resolve outside the root (canonicalize on the MCP server side).
- **Context budget:** every tool result is head+tail truncated; when the transcript exceeds roughly 70% of the window, replace the oldest tool results with one-line summaries (add this as a pre-turn step in `turn()`).
- **Determinism:** never let Jev or the model author the verification command list; the harness derives it from project files.
- **Secrets:** exclude `.env*` and key files from `read_file` by deny-pattern.

## 9. Build order and acceptance tests

1. **error, config, events.** Test: `validate()` rejects a non-loopback URL without the `remote` feature.
2. **MCP client.** Test against a small echo MCP server: initialize handshake, `tools/call` success, `isError` mapping, server crash returns an `is_error` outcome (no panic), timeout path.
3. **AgentTool::parse.** Table tests: `Read_File`, `READFILE`, `file_path` alias, missing argument yields a teaching error, unknown tool yields a list of valid tools.
4. **Wiki load/compile.** Missing file gives an empty context; frontmatter round-trip; atomic write leaves no `.state.tmp`; compile with a stub LLM.
5. **Jev adapter.** Mock HTTP server: out-of-range index, NaN score, wrong score length, and timeout all produce `Escalate`, `Uncertain`, or `None`.
6. **LLM client.** Replay recorded SSE streams: UTF-8 split across chunks, fragmented `tool_calls` deltas, missing ids, `[DONE]`.
7. **Orchestrator.** Scripted fake backend: happy path to `Done`; failing tests loop through Act then pass; retries exhausted gives `Blocked`; no detectable verifier gives `Blocked`; a model claiming done with failing tests is never accepted.
8. **TUI.** Smoke test `draw` with `ratatui::backend::TestBackend`; manual run for key handling and Esc interrupt.
9. **End to end.** Run against a throwaway repo with `llama-server`; confirm a second session starts from the compiled wiki and uses a smaller initial context than the first.

## 10. Known risks

- **Jev contract mismatch** is the biggest unknown. The adapter isolates it, but thresholds (`min_route_conf`, the 0.8 noul threshold) need calibration on real traces.
- **Local tool-call quality** varies by model and chat template. If malformed calls persist, add grammar or JSON-schema constrained decoding in `llama-server` for tool-call turns.
- **Wiki drift.** The compile step is model-written. Keep the deterministic digest in the prompt and add a lint step (reject output missing the four required headings, retry once).
- **Crate API drift** (`gray_matter`, `ratatui`, `reqwest` features): pin versions in `Cargo.lock` and treat compile errors at those call sites as expected adjustments.

