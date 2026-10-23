//! Read-only filesystem MCP server binary.
//!
//! Usage: `filesystem_mcp --root <repository-root>`.
//! Protocol traffic uses stdin/stdout; diagnostics use stderr.

#![forbid(unsafe_code)]

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use local_agent_harness::mcp::fs_server::{ReadOnlyFilesystem, serve};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("filesystem_mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    match parse_args(std::env::args().skip(1))? {
        Command::Help => {
            println!("usage: filesystem_mcp --root <repository-root>");
            Ok(())
        }
        Command::Serve(root) => serve_root(root),
    }
}

fn serve_root(root: PathBuf) -> Result<(), String> {
    let server = ReadOnlyFilesystem::new(&root)
        .map_err(|error| format!("cannot open repository root {}: {error}", root.display()))?;

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serve(&server, BufReader::new(stdin.lock()), &mut output)
        .map_err(|error| format!("protocol I/O failed: {error}"))
}

enum Command {
    Serve(PathBuf),
    Help,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if root.is_some() {
                    return Err("`--root` may be provided only once".into());
                }
                let value = args.next().ok_or("`--root` requires a repository path")?;
                root = Some(PathBuf::from(value));
            }
            "--help" | "-h" => return Ok(Command::Help),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    root.map(Command::Serve)
        .ok_or_else(|| "missing required `--root <repository-root>`".into())
}
