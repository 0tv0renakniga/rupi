//! Test-support MCP server.
//!
//! This binary is used only by integration tests. It is not linked into or
//! spawned by the harness runtime. It speaks newline-delimited JSON-RPC 2.0
//! over stdin/stdout and provides deterministic success, failure, timeout,
//! exit, notification, protocol-error, and server-request scenarios.

use std::io::{self, BufRead, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value, json};

fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    write_pid_file(&args)?;
    write_env_file(&args)?;
    write_network_probe(&args)?;
    write_startup_stderr(&args)?;

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout();

    while let Some(message) = read_message(&mut input)? {
        handle_message(&message, &mut input, &mut output)?;
    }
    Ok(())
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn write_pid_file(args: &[String]) -> io::Result<()> {
    let Some(path) = arg_value(args, "--pid-file") else {
        return Ok(());
    };
    std::fs::write(path, std::process::id().to_string())
}

fn write_env_file(args: &[String]) -> io::Result<()> {
    let Some(path) = arg_value(args, "--env-file") else {
        return Ok(());
    };
    let names = arg_value(args, "--env-names")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let mut values = Map::new();
    for name in names {
        let value = std::env::var_os(name)
            .map(|value| Value::String(value.to_string_lossy().into_owned()))
            .unwrap_or(Value::Null);
        values.insert(name.to_owned(), value);
    }
    std::fs::write(path, Value::Object(values).to_string())
}

fn write_network_probe(args: &[String]) -> io::Result<()> {
    let Some(raw_addr) = arg_value(args, "--network-probe-addr") else {
        return Ok(());
    };
    let Some(path) = arg_value(args, "--network-probe-result-file") else {
        return Ok(());
    };
    let addr: SocketAddr = raw_addr.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid --network-probe-addr: {error}"),
        )
    })?;
    let connected = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok();
    let (dns_tx, dns_rx) = mpsc::channel();
    thread::spawn(move || {
        let resolved = ("example.com", 80)
            .to_socket_addrs()
            .is_ok_and(|mut addresses| addresses.next().is_some());
        let _ = dns_tx.send(resolved);
    });
    let dns_resolved = dns_rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap_or(false);
    let routes = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let devices = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let report = json!({
        "tcp_loopback": connected,
        "dns_resolved": dns_resolved,
        "resolv_conf_present": std::path::Path::new("/etc/resolv.conf").exists(),
        "routes": routes,
        "devices": devices,
    });
    std::fs::write(path, report.to_string())
}

fn write_startup_stderr(args: &[String]) -> io::Result<()> {
    let raw = arg_value(args, "--startup-stderr-bytes")
        .map(str::to_owned)
        .or_else(|| std::env::var("MOCK_STDERR_BYTES").ok());
    let Some(raw) = raw else {
        return Ok(());
    };
    let Ok(mut remaining) = raw.parse::<usize>() else {
        return Ok(());
    };

    let mut stderr = io::stderr().lock();
    if let Some(text) = arg_value(args, "--startup-stderr-text") {
        stderr.write_all(text.as_bytes())?;
        stderr.write_all(b"\n")?;
    }
    let chunk = [b'S'; 4_096];
    while remaining > 0 {
        let write = remaining.min(chunk.len());
        stderr.write_all(&chunk[..write])?;
        remaining -= write;
    }
    stderr.flush()
}

fn read_message<R: BufRead>(input: &mut R) -> io::Result<Option<Value>> {
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let value = serde_json::from_str(line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        return Ok(Some(value));
    }
}

fn write_message<W: Write>(output: &mut W, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    output.write_all(b"\n")?;
    output.flush()
}

fn handle_message<R: BufRead, W: Write>(
    message: &Value,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");

    match method {
        "initialize" => {
            if let Some(delay) = initialize_delay_ms() {
                thread::sleep(Duration::from_millis(delay));
            }
            let requested_version = message
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            send_result(
                output,
                &id,
                json!({
                    "protocolVersion": requested_version,
                    "capabilities": {},
                    "serverInfo": {"name": "mock-mcp-server", "version": "0.0.0"},
                }),
            )
        }
        "notifications/initialized" => Ok(()),
        "tools/list" => {
            let tools = advertised_tools()
                .into_iter()
                .map(|name| tool_descriptor(&name))
                .collect::<Vec<_>>();
            send_result(output, &id, json!({ "tools": tools }))
        }
        "tools/call" => handle_tool_call(message, input, output),
        _ => {
            if id.is_null() {
                Ok(())
            } else {
                send_error(output, &id, -32601, "Method not found")
            }
        }
    }
}

fn initialize_delay_ms() -> Option<u64> {
    arg_value(
        &std::env::args().skip(1).collect::<Vec<_>>(),
        "--initialize-delay-ms",
    )
    .and_then(|value| value.parse().ok())
    .map(|value: u64| value.min(60_000))
}

const DEFAULT_ADVERTISED_TOOLS: &[&str] = &[
    "echo",
    "fail",
    "delay",
    "exit",
    "noise",
    "server_request",
    "malformed",
    "stderr",
    "env",
    "read_file",
    "write_file",
    "apply_patch",
    "run",
    "status",
    "diff",
];

fn advertised_tools() -> Vec<String> {
    let raw = arg_value(&std::env::args().skip(1).collect::<Vec<_>>(), "--tools")
        .map(str::to_owned)
        .or_else(|| std::env::var("MOCK_ADVERTISED_TOOLS").ok());
    let Some(raw) = raw else {
        return DEFAULT_ADVERTISED_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
    };

    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn tool_descriptor(name: &str) -> Value {
    let description = match name {
        "echo" => "Echo the supplied text.",
        "fail" => "Return an isError result.",
        "read_file" => "Read a repository-relative file.",
        "write_file" => "Write a repository-relative file.",
        "apply_patch" => "Apply a unified diff to a file.",
        "run" => "Run an allowlisted program with argv.",
        "status" => "Return Git status.",
        "diff" => "Return a Git diff.",
        "env" => "Report selected child environment variables.",
        _ => "Deterministic mock tool.",
    };

    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object"}
    })
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn route_tool_text(name: &str, arguments: &Value) -> String {
    let mut text = json!({
        "tool": name,
        "arguments": arguments,
    })
    .to_string();

    if let Ok(raw) = std::env::var("MOCK_TOOL_OUTPUT_BYTES")
        && let Ok(bytes) = raw.parse::<usize>()
    {
        text.push_str(&"X".repeat(bytes));
    }
    text
}

fn handle_tool_call<R: BufRead, W: Write>(
    message: &Value,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "echo" => {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("ok");
            send_tool_result(output, &id, text, false)
        }
        "fail" => {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("intentional failure");
            send_tool_result(output, &id, message, true)
        }
        "delay" => {
            let millis = arguments
                .get("millis")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(60_000);
            thread::sleep(Duration::from_millis(millis));
            send_tool_result(output, &id, "delayed", false)
        }
        "exit" => std::process::exit(0),
        "noise" => {
            write_message(
                output,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 999_999,
                    "result": {
                        "content": [{"type": "text", "text": "unknown response"}],
                        "isError": false
                    }
                }),
            )?;
            write_message(
                output,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/mock",
                    "params": {"note": "ignored safely"}
                }),
            )?;
            send_tool_result(output, &id, "noise-ok", false)
        }
        "server_request" => {
            write_message(
                output,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 9_001,
                    "method": "mock/ping",
                    "params": {}
                }),
            )?;

            let Some(reply) = read_message(input)? else {
                std::process::exit(0);
            };
            let code = reply
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_i64);
            let text = match code {
                Some(-32601) => "server request rejected with -32601".to_owned(),
                _ => format!("unexpected server-request reply: {reply}"),
            };
            send_tool_result(output, &id, &text, false)
        }
        "malformed" => {
            output.write_all(b"{this is not valid json\n")?;
            output.flush()
        }
        "stderr" => {
            let bytes = arguments
                .get("bytes")
                .and_then(Value::as_u64)
                .unwrap_or(4_096) as usize;
            let chunk = [b'E'; 1_024];
            let mut stderr = io::stderr().lock();
            let mut remaining = bytes;
            while remaining > 0 {
                let write = remaining.min(chunk.len());
                stderr.write_all(&chunk[..write])?;
                remaining -= write;
            }
            stderr.flush()?;
            send_tool_result(output, &id, "stderr-written", false)
        }
        "env" => {
            let names = arguments
                .get("names")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut values = Map::new();
            for name in names {
                let value = std::env::var_os(name)
                    .map(|value| Value::String(value.to_string_lossy().into_owned()))
                    .unwrap_or(Value::Null);
                values.insert(name.to_owned(), value);
            }
            send_tool_result(output, &id, &Value::Object(values).to_string(), false)
        }
        "read_file" | "write_file" | "apply_patch" | "run" | "status" | "diff" => {
            if env_list("MOCK_EXIT_TOOLS").iter().any(|tool| tool == name) {
                std::process::exit(0);
            }

            if env_list("MOCK_ERROR_TOOLS").iter().any(|tool| tool == name) {
                let text = format!("server-declared failure for `{name}`: {}", arguments);
                return send_tool_result(output, &id, &text, true);
            }

            if std::env::args().any(|arg| arg == "--real-filesystem")
                && matches!(name, "read_file" | "write_file" | "apply_patch")
            {
                return handle_real_filesystem(output, &id, name, &arguments);
            }

            send_tool_result(output, &id, &route_tool_text(name, &arguments), false)
        }
        _ => send_tool_result(output, &id, &format!("unknown mock tool `{name}`"), true),
    }
}

fn handle_real_filesystem<W: Write>(
    output: &mut W,
    id: &Value,
    name: &str,
    arguments: &Value,
) -> io::Result<()> {
    let Some(raw_path) = arguments.get("path").and_then(Value::as_str) else {
        return send_tool_result(output, id, "missing path", true);
    };
    let Ok(root) = std::env::current_dir() else {
        return send_tool_result(output, id, "missing workspace", true);
    };
    let mut relative = std::path::PathBuf::new();
    for component in std::path::Path::new(raw_path).components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            _ => return send_tool_result(output, id, "path rejected", true),
        }
    }
    if relative.as_os_str().is_empty() {
        return send_tool_result(output, id, "empty path", true);
    }
    let path = root.join(relative);

    match name {
        "read_file" => match std::fs::read_to_string(&path) {
            Ok(content) => send_tool_result(output, id, &content, false),
            Err(error) => send_tool_result(output, id, &format!("read failed: {error}"), true),
        },
        "write_file" => {
            let content = arguments
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = path
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| std::fs::write(&path, content));
            match result {
                Ok(()) => send_tool_result(
                    output,
                    id,
                    &format!("wrote {raw_path} token=MCP_E2E_SECRET"),
                    false,
                ),
                Err(error) => send_tool_result(output, id, &format!("write failed: {error}"), true),
            }
        }
        "apply_patch" => {
            let patch = arguments
                .get("unified_diff")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match std::fs::write(&path, patch) {
                Ok(()) => send_tool_result(output, id, &format!("patched {raw_path}"), false),
                Err(error) => send_tool_result(output, id, &format!("patch failed: {error}"), true),
            }
        }
        _ => send_tool_result(output, id, "unsupported real filesystem tool", true),
    }
}

fn send_result<W: Write>(output: &mut W, id: &Value, result: Value) -> io::Result<()> {
    write_message(
        output,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    )
}

fn send_error<W: Write>(output: &mut W, id: &Value, code: i64, message: &str) -> io::Result<()> {
    write_message(
        output,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }),
    )
}

fn send_tool_result<W: Write>(
    output: &mut W,
    id: &Value,
    text: &str,
    is_error: bool,
) -> io::Result<()> {
    send_result(
        output,
        id,
        json!({
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
        }),
    )
}
