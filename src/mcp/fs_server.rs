//! Read-only filesystem MCP server.
//!
//! This module implements the production read-only `filesystem` slot. It
//! advertises exactly one tool, `read_file`, and never opens a file for writing.
//! The binary wrapper is `src/bin/filesystem_mcp.rs`; keeping the protocol and
//! path logic here makes it directly unit-testable.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

/// MCP revision implemented by this server.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Stable server name reported in `serverInfo`.
pub const SERVER_NAME: &str = "filesystem-mcp";
/// Maximum bytes returned by one `read_file` call.
pub const MAX_READ_BYTES: usize = 64 * 1024;
/// Maximum bytes in one inbound JSON-RPC request line, excluding the newline.
pub const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

const JSONRPC_VERSION: &str = "2.0";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// A filesystem server pinned to one canonical repository root.
#[derive(Debug, Clone)]
pub struct ReadOnlyFilesystem {
    root: PathBuf,
}

impl ReadOnlyFilesystem {
    /// Canonicalize and validate the fixed root. The root is supplied by
    /// configuration, never by a model request.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem MCP root must be a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read a repository-relative file, optionally selecting an inclusive
    /// 1-based line range.
    pub fn read_file(&self, arguments: &Value) -> Result<String, String> {
        let raw_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or("read_file requires string field `path`")?;
        let line_start = optional_line(arguments, "line_start")?;
        let line_end = optional_line(arguments, "line_end")?;
        if let (Some(start), Some(end)) = (line_start, line_end)
            && end < start
        {
            return Err("`line_end` must be greater than or equal to `line_start`".into());
        }

        let path = self.resolve_existing_file(raw_path)?;
        let file = File::open(&path).map_err(|error| format!("cannot open file: {error}"))?;
        let mut reader = BufReader::new(file);
        let mut output = Vec::new();
        let mut line_number = 1_u32;

        loop {
            let buffer = reader
                .fill_buf()
                .map_err(|error| format!("cannot read file: {error}"))?;
            if buffer.is_empty() {
                break;
            }

            let mut segment_start = 0;
            for (index, byte) in buffer.iter().enumerate() {
                if *byte != b'\n' {
                    continue;
                }
                append_line(
                    &mut output,
                    &buffer[segment_start..=index],
                    line_number,
                    line_start,
                    line_end,
                )?;
                segment_start = index + 1;
                if line_end.is_some_and(|end| line_number >= end) {
                    return utf8_output(output);
                }
                line_number = line_number.saturating_add(1);
            }
            if segment_start < buffer.len() {
                append_line(
                    &mut output,
                    &buffer[segment_start..],
                    line_number,
                    line_start,
                    line_end,
                )?;
            }
            let consumed = buffer.len();
            reader.consume(consumed);
        }

        utf8_output(output)
    }

    fn resolve_existing_file(&self, raw: &str) -> Result<PathBuf, String> {
        if raw.is_empty() {
            return Err("path must not be empty".into());
        }
        if raw.contains('\0') {
            return Err("path must not contain NUL bytes".into());
        }

        let path = Path::new(raw);
        if path.is_absolute() {
            return Err("absolute paths are not allowed".into());
        }

        let mut relative = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => relative.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err("path traversal (`..`) is not allowed".into());
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err("absolute paths are not allowed".into());
                }
            }
        }
        if relative.as_os_str().is_empty() {
            return Err("path must name a file".into());
        }

        let candidate = self.root.join(relative);
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("cannot resolve path: {error}"))?;
        if !canonical.starts_with(&self.root) {
            return Err("path escapes the repository root".into());
        }
        if !canonical.is_file() {
            return Err("path is not a regular file".into());
        }
        Ok(canonical)
    }
}

fn append_line(
    output: &mut Vec<u8>,
    line: &[u8],
    line_number: u32,
    line_start: Option<u32>,
    line_end: Option<u32>,
) -> Result<(), String> {
    if line_start.is_some_and(|start| line_number < start)
        || line_end.is_some_and(|end| line_number > end)
    {
        return Ok(());
    }
    if output.len().saturating_add(line.len()) > MAX_READ_BYTES {
        return Err(format!(
            "selected file content exceeds the {MAX_READ_BYTES}-byte output bound; use `line_start` and `line_end`"
        ));
    }
    output.extend_from_slice(line);
    Ok(())
}

fn utf8_output(output: Vec<u8>) -> Result<String, String> {
    String::from_utf8(output)
        .map_err(|error| format!("cannot read file as UTF-8 text: {}", error.utf8_error()))
}

fn optional_line(arguments: &Value, key: &str) -> Result<Option<u32>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let raw = value
        .as_u64()
        .ok_or_else(|| format!("`{key}` must be a positive integer"))?;
    if raw == 0 {
        return Err(format!("`{key}` must be greater than zero"));
    }
    u32::try_from(raw)
        .map(Some)
        .map_err(|_| format!("`{key}` exceeds the supported line range"))
}

/// Serve the MCP protocol until the input stream closes.
///
/// All protocol output goes to `output`; startup diagnostics belong on stderr.
pub fn serve<R: BufRead, W: Write>(
    server: &ReadOnlyFilesystem,
    mut input: R,
    output: &mut W,
) -> io::Result<()> {
    loop {
        let line = match read_bounded_line(&mut input, MAX_REQUEST_LINE_BYTES)? {
            BoundedLine::Eof => return Ok(()),
            BoundedLine::Line(line) => line,
            BoundedLine::TooLarge => {
                send_error(
                    output,
                    Value::Null,
                    INVALID_REQUEST,
                    format!("Request line exceeds the {MAX_REQUEST_LINE_BYTES}-byte maximum"),
                )?;
                continue;
            }
        };
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        handle_line(server, &line, output)?;
    }
}

enum BoundedLine {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

fn read_bounded_line<R: BufRead>(input: &mut R, max: usize) -> io::Result<BoundedLine> {
    let mut line = Vec::new();
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(BoundedLine::Eof)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }

        if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let content_len = line.len().saturating_add(newline);
            let consume_len = newline + 1;
            if content_len > max {
                input.consume(consume_len);
                return Ok(BoundedLine::TooLarge);
            }
            line.extend_from_slice(&buffer[..consume_len]);
            input.consume(consume_len);
            return Ok(BoundedLine::Line(line));
        }

        if line.len().saturating_add(buffer.len()) > max {
            let consume_len = buffer.len();
            input.consume(consume_len);
            drain_to_newline(input)?;
            return Ok(BoundedLine::TooLarge);
        }

        line.extend_from_slice(buffer);
        let consume_len = buffer.len();
        input.consume(consume_len);
    }
}

fn drain_to_newline<R: BufRead>(input: &mut R) -> io::Result<()> {
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let consume_len = newline + 1;
            input.consume(consume_len);
            return Ok(());
        }
        let consume_len = buffer.len();
        input.consume(consume_len);
    }
}

fn handle_line<W: Write>(
    server: &ReadOnlyFilesystem,
    line: &[u8],
    output: &mut W,
) -> io::Result<()> {
    let message: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(_) => {
            return send_error(output, Value::Null, PARSE_ERROR, "Parse error");
        }
    };

    let Some(object) = message.as_object() else {
        return send_error(output, Value::Null, INVALID_REQUEST, "Invalid Request");
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return send_error(
            output,
            object.get("id").cloned().unwrap_or(Value::Null),
            INVALID_REQUEST,
            "Invalid Request: unsupported or missing jsonrpc version",
        );
    }

    let id = object.get("id").cloned().filter(|value| !value.is_null());
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return send_error(
            output,
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "Invalid Request: missing method",
        );
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let Some(id) = id else {
                return Ok(());
            };
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if requested != PROTOCOL_VERSION {
                return send_error(
                    output,
                    id,
                    INVALID_PARAMS,
                    format!("unsupported protocol version `{requested}`"),
                );
            }
            send_result(
                output,
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
        }
        "notifications/initialized" => Ok(()),
        "ping" => match id {
            Some(id) => send_result(output, id, json!({})),
            None => Ok(()),
        },
        "tools/list" => match id {
            Some(id) => send_result(
                output,
                id,
                json!({
                    "tools": [{
                        "name": "read_file",
                        "description": "Read a repository-relative UTF-8 file, optionally selecting an inclusive 1-based line range.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Repository-relative file path."
                                },
                                "line_start": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "First line to return (1-based, inclusive)."
                                },
                                "line_end": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Last line to return (1-based, inclusive)."
                                }
                            },
                            "required": ["path"],
                            "additionalProperties": false
                        }
                    }]
                }),
            ),
            None => Ok(()),
        },
        "tools/call" => match id {
            Some(id) => handle_tool_call(server, id, &params, output),
            None => Ok(()),
        },
        other => match id {
            Some(id) => send_error(
                output,
                id,
                METHOD_NOT_FOUND,
                format!("Method not found: `{other}`"),
            ),
            None => Ok(()),
        },
    }
}

fn handle_tool_call<W: Write>(
    server: &ReadOnlyFilesystem,
    id: Value,
    params: &Value,
    output: &mut W,
) -> io::Result<()> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let (text, is_error) = if name == "read_file" {
        match server.read_file(&arguments) {
            Ok(text) => (text, false),
            Err(error) => (error, true),
        }
    } else {
        (format!("unknown tool `{name}`"), true)
    };

    send_result(
        output,
        id,
        json!({
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
        }),
    )
}

fn send_result<W: Write>(output: &mut W, id: Value, result: Value) -> io::Result<()> {
    write_message(
        output,
        &json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "result": result,
        }),
    )
}

fn send_error<W: Write>(
    output: &mut W,
    id: Value,
    code: i64,
    message: impl Into<String>,
) -> io::Result<()> {
    write_message(
        output,
        &json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "error": {"code": code, "message": message.into()},
        }),
    )
}

fn write_message<W: Write>(output: &mut W, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("rupi-fs-server-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }

        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::write(&path, content).expect("write fixture");
            path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn server(root: &TempRoot) -> ReadOnlyFilesystem {
        ReadOnlyFilesystem::new(&root.path).expect("create server")
    }

    #[test]
    fn reads_a_file_without_modifying_it() {
        let root = TempRoot::new("read");
        let path = root.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n");
        let before = std::fs::read(&path).expect("read before");

        let text = server(&root)
            .read_file(&json!({"path": "src/lib.rs"}))
            .expect("read file");

        assert_eq!(text, "pub fn value() -> u8 { 1 }\n");
        assert_eq!(std::fs::read(&path).expect("read after"), before);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let root = TempRoot::new("unsafe");
        root.write("ok.txt", "ok\n");
        let server = server(&root);

        for path in [
            "",
            ".",
            "..",
            "../outside",
            "/etc/passwd",
            "nested/../../outside",
            "nul\0path",
        ] {
            let error = server
                .read_file(&json!({"path": path}))
                .expect_err("unsafe path must fail");
            assert!(!error.is_empty(), "empty error for {path:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_but_allows_symlink_inside_root() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new("symlink");
        let outside = TempRoot::new("outside");
        let outside_file = outside.write("secret.txt", "outside\n");
        let inside_file = root.write("inside.txt", "inside\n");
        symlink(&outside_file, root.path.join("escape.txt")).expect("create escape symlink");
        symlink(&inside_file, root.path.join("inside-link.txt")).expect("create inside symlink");

        let server = server(&root);
        let error = server
            .read_file(&json!({"path": "escape.txt"}))
            .expect_err("escaping symlink must fail");
        assert!(error.contains("escapes the repository root"), "{error}");

        let text = server
            .read_file(&json!({"path": "inside-link.txt"}))
            .expect("inside symlink");
        assert_eq!(text, "inside\n");
    }

    #[test]
    fn line_ranges_are_one_based_and_inclusive() {
        let root = TempRoot::new("lines");
        root.write("lines.txt", "one\ntwo\nthree\n");
        let server = server(&root);

        assert_eq!(
            server
                .read_file(&json!({"path": "lines.txt", "line_start": 2, "line_end": 2}))
                .expect("middle line"),
            "two\n"
        );
        assert_eq!(
            server
                .read_file(&json!({"path": "lines.txt", "line_start": 2}))
                .expect("from line two"),
            "two\nthree\n"
        );
        assert_eq!(
            server
                .read_file(&json!({"path": "lines.txt", "line_end": 2}))
                .expect("through line two"),
            "one\ntwo\n"
        );
        assert!(
            server
                .read_file(&json!({"path": "lines.txt", "line_start": 3, "line_end": 2}))
                .is_err()
        );
        assert!(
            server
                .read_file(&json!({"path": "lines.txt", "line_start": 0}))
                .is_err()
        );
    }

    #[test]
    fn line_ranges_skip_large_unselected_lines() {
        let root = TempRoot::new("skip-large");
        let mut content = "x".repeat(MAX_READ_BYTES * 2);
        content.push('\n');
        content.push_str("small\n");
        root.write("mixed.txt", &content);

        let text = server(&root)
            .read_file(&json!({"path": "mixed.txt", "line_start": 2, "line_end": 2}))
            .expect("select small line");
        assert_eq!(text, "small\n");
    }

    #[test]
    fn output_is_bounded() {
        let root = TempRoot::new("bound");
        root.write("large.txt", &"x".repeat(MAX_READ_BYTES + 1));
        let error = server(&root)
            .read_file(&json!({"path": "large.txt"}))
            .expect_err("oversized read must fail");
        assert!(error.contains("output bound"), "{error}");
    }

    fn protocol(root: &TempRoot, requests: &[Value]) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve(&server(root), Cursor::new(input.as_bytes()), &mut output).expect("serve protocol");
        String::from_utf8(output)
            .expect("utf-8 protocol output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse protocol response"))
            .collect()
    }

    #[test]
    fn advertises_only_read_file_and_serves_calls() {
        let root = TempRoot::new("protocol");
        root.write("README.md", "hello\n");
        let responses = protocol(
            &root,
            &[
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": PROTOCOL_VERSION}
                }),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
                json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {"name": "read_file", "arguments": {"path": "README.md"}}
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {"name": "write_file", "arguments": {"path": "README.md"}}
                }),
            ],
        );

        assert_eq!(responses.len(), 4);
        assert_eq!(responses[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], SERVER_NAME);
        let tools = responses[1]["result"]["tools"]
            .as_array()
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "read_file");
        assert_eq!(responses[2]["result"]["isError"], false);
        assert_eq!(responses[2]["result"]["content"][0]["text"], "hello\n");
        assert_eq!(responses[3]["result"]["isError"], true);
        assert!(
            responses[3]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    #[test]
    fn accepts_request_line_at_the_limit() {
        let root = TempRoot::new("request-limit");
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        })
        .to_string();
        assert!(request.len() <= MAX_REQUEST_LINE_BYTES);
        let mut input = request.into_bytes();
        let padding = MAX_REQUEST_LINE_BYTES - input.len();
        input.extend_from_slice(" ".repeat(padding).as_bytes());
        input.push(b'\n');

        let mut output = Vec::new();
        serve(&server(&root), Cursor::new(input), &mut output).expect("serve at-limit request");
        let response: Value =
            serde_json::from_slice(output.strip_suffix(b"\n").expect("trailing newline"))
                .expect("parse response");
        assert_eq!(response["result"]["tools"][0]["name"], "read_file");
    }

    #[test]
    fn rejects_oversized_request_line_and_recovers() {
        let root = TempRoot::new("request-oversized");
        let mut input = vec![b'x'; MAX_REQUEST_LINE_BYTES + 1];
        input.push(b'\n');
        input.extend_from_slice(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            })
            .to_string()
            .as_bytes(),
        );
        input.push(b'\n');

        let mut output = Vec::new();
        serve(&server(&root), Cursor::new(input), &mut output).expect("serve oversized request");
        let responses: Vec<Value> = String::from_utf8(output)
            .expect("utf-8 protocol output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse protocol response"))
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["error"]["code"], INVALID_REQUEST);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("maximum")
        );
        assert_eq!(responses[1]["result"]["tools"][0]["name"], "read_file");
    }

    #[test]
    fn rejects_unsupported_protocol_and_unknown_methods() {
        let root = TempRoot::new("errors");
        let responses = protocol(
            &root,
            &[
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": "2099-01-01"}
                }),
                json!({"jsonrpc": "2.0", "id": 2, "method": "unknown/method"}),
            ],
        );
        assert_eq!(responses[0]["error"]["code"], INVALID_PARAMS);
        assert_eq!(responses[1]["error"]["code"], METHOD_NOT_FOUND);
    }
}
