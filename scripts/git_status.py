"""Bounded read-only Git status helper for the versioned script protocol.

The helper reads one JSON request from stdin and writes exactly one JSON
response to stdout. It runs a fixed argv-only ``git status`` command in the
configured root with a minimal environment, no pager, and optional Git locks
disabled. Status output is bounded and made display-safe before it is returned.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, BinaryIO

from path_policy import PathPolicy, PathPolicyError

PROTOCOL_VERSION = 1
ACTION = "git_status"
MAX_REQUEST_BYTES = 64 * 1024
MAX_STATUS_BYTES = 64 * 1024
MAX_STDERR_BYTES = 8 * 1024
MAX_ERROR_MESSAGE_BYTES = 1024
GIT_TIMEOUT_SECONDS = 30
_TRUNCATION_MARKER = "...[truncated]"
_ALLOWED_REQUEST_FIELDS = frozenset(
    {"protocol_version", "action", "project_root", "params"}
)
_INHERITED_ENV_KEYS = (
    "PATH",
    "HOME",
    "USERPROFILE",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "TEMP",
    "TMP",
)


class HelperError(ValueError):
    """Structured helper failure with a stable machine-readable code."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        self.detail = message
        super().__init__(f"[{code}] {message}")


def main() -> int:
    response = handle_request()
    json.dump(response, sys.stdout, ensure_ascii=False, separators=(",", ":"))
    return 0


def handle_request() -> dict[str, Any]:
    try:
        raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
        if len(raw) > MAX_REQUEST_BYTES:
            raise HelperError(
                "REQUEST_TOO_LARGE",
                "script request exceeds the configured size bound",
            )
        request = _parse_json_object(raw)
        return _execute(request)
    except HelperError as error:
        return _error_response(error.code, error.detail)
    except PathPolicyError as error:
        return _error_response(error.code, error.detail)
    except Exception:
        return _error_response(
            "GIT_STATUS_FAILED", "git status helper failed to process the request"
        )


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise HelperError("INVALID_REQUEST", "request must be valid JSON") from error
    if not isinstance(value, dict):
        raise HelperError("INVALID_REQUEST", "request must be a JSON object")
    return value


def _execute(request: dict[str, Any]) -> dict[str, Any]:
    unknown = set(request) - _ALLOWED_REQUEST_FIELDS
    if unknown:
        raise HelperError("INVALID_REQUEST", "request contains unknown fields")

    version = request.get("protocol_version")
    if isinstance(version, bool) or not isinstance(version, int):
        raise HelperError("INVALID_REQUEST", "protocol_version must be an integer")
    if version != PROTOCOL_VERSION:
        raise HelperError(
            "UNSUPPORTED_PROTOCOL_VERSION",
            f"unsupported script protocol version: {version}",
        )

    if request.get("action") != ACTION:
        raise HelperError("INVALID_ACTION", "helper only accepts action `git_status`")

    project_root = request.get("project_root")
    if not isinstance(project_root, str) or not project_root:
        raise HelperError("INVALID_REQUEST", "project_root must be a non-empty string")

    params = request.get("params", {})
    if not isinstance(params, dict):
        raise HelperError("INVALID_PARAMS", "params must be a JSON object")
    if params:
        raise HelperError("INVALID_PARAMS", "git_status does not accept parameters")

    policy = PathPolicy(project_root)
    status, truncated = _run_git_status(policy.root)

    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": True,
        "data": {
            "clean": not status and not truncated,
            "status": status,
            "truncated": truncated,
        },
    }


def _run_git_status(root: Path) -> tuple[str, bool]:
    environment = _git_environment()
    git = shutil.which("git", path=environment["PATH"])
    if git is None:
        raise HelperError("GIT_UNAVAILABLE", "git executable is unavailable")

    args = [
        git,
        "--no-pager",
        "-c",
        "core.quotePath=true",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "status.relativePaths=false",
        "-c",
        "color.ui=false",
        "status",
        "--porcelain=v1",
        "--untracked-files=normal",
        "--",
        ".",
    ]
    try:
        process = subprocess.Popen(
            args,
            cwd=root,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise HelperError("GIT_UNAVAILABLE", "git could not be started") from error

    results: dict[str, tuple[bytes, bool]] = {}
    read_errors: list[OSError] = []
    threads = [
        threading.Thread(
            target=_collect_stream,
            args=("stdout", process.stdout, MAX_STATUS_BYTES, results, read_errors),
            daemon=True,
        ),
        threading.Thread(
            target=_collect_stream,
            args=("stderr", process.stderr, MAX_STDERR_BYTES, results, read_errors),
            daemon=True,
        ),
    ]
    for thread in threads:
        thread.start()

    try:
        process.wait(timeout=GIT_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        _terminate(process)
        _join_threads(threads)
        raise HelperError(
            "GIT_TIMEOUT", "git status exceeded the configured timeout"
        ) from error

    if not _join_threads(threads):
        _terminate(process)
        raise HelperError("GIT_STATUS_FAILED", "git status output could not be collected")
    if read_errors:
        raise HelperError("GIT_STATUS_FAILED", "git status output could not be collected")

    stdout, stdout_truncated = results.get("stdout", (b"", False))
    stderr, stderr_truncated = results.get("stderr", (b"", False))
    if process.returncode != 0:
        diagnostic = _diagnostic(stderr, stderr_truncated)
        if "not a git repository" in diagnostic.lower():
            raise HelperError(
                "NOT_A_GIT_REPOSITORY",
                "configured root is not a Git repository",
            )
        message = diagnostic or f"git status exited with status {process.returncode}"
        raise HelperError("GIT_STATUS_FAILED", message)

    return _bounded_status(stdout, stdout_truncated)


def _collect_stream(
    name: str,
    stream: BinaryIO | None,
    cap: int,
    results: dict[str, tuple[bytes, bool]],
    errors: list[OSError],
) -> None:
    if stream is None:
        results[name] = (b"", False)
        return
    try:
        results[name] = _read_bounded(stream, cap)
    except OSError as error:
        errors.append(error)


def _read_bounded(stream: BinaryIO, cap: int) -> tuple[bytes, bool]:
    data = bytearray()
    truncated = False
    while True:
        chunk = stream.read(8192)
        if not chunk:
            break
        remaining = cap - len(data)
        if remaining <= 0:
            truncated = True
            continue
        if len(chunk) > remaining:
            data.extend(chunk[:remaining])
            truncated = True
        else:
            data.extend(chunk)
    return bytes(data), truncated


def _join_threads(threads: list[threading.Thread]) -> bool:
    for thread in threads:
        thread.join(timeout=5)
    return all(not thread.is_alive() for thread in threads)


def _terminate(process: subprocess.Popen[bytes]) -> None:
    try:
        process.kill()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass


def _git_environment() -> dict[str, str]:
    environment = {
        key: os.environ[key] for key in _INHERITED_ENV_KEYS if key in os.environ
    }
    if not environment.get("PATH"):
        environment["PATH"] = os.defpath
    environment.update(
        {
            "LC_ALL": "C",
            "LANG": "C",
            "GIT_OPTIONAL_LOCKS": "0",
            "GIT_PAGER": "cat",
            "PAGER": "cat",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_SYSTEM": os.devnull,
        }
    )
    return environment


def _bounded_status(raw: bytes, truncated: bool) -> tuple[str, bool]:
    text = raw.decode("utf-8", errors="replace")
    safe = "".join(
        character
        if character in "\n\t" or character.isprintable()
        else " "
        for character in text
    )
    encoded = safe.encode("utf-8")
    if len(encoded) <= MAX_STATUS_BYTES:
        return safe, truncated
    return encoded[:MAX_STATUS_BYTES].decode("utf-8", errors="ignore"), True


def _diagnostic(stderr: bytes, truncated: bool) -> str:
    text = stderr.decode("utf-8", errors="replace")
    if truncated:
        text = "[stderr truncated]\n" + text
    return _bounded_message(text)


def _error_response(code: str, message: str) -> dict[str, Any]:
    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": False,
        "error": {"code": code, "message": _bounded_message(message)},
    }


def _bounded_message(message: str) -> str:
    sanitized = "".join(
        character if character.isprintable() else " " for character in str(message)
    ).strip()
    if not sanitized:
        return "git status helper reported an error without a message"

    encoded = sanitized.encode("utf-8")
    if len(encoded) <= MAX_ERROR_MESSAGE_BYTES:
        return sanitized
    if MAX_ERROR_MESSAGE_BYTES <= len(_TRUNCATION_MARKER):
        return encoded[:MAX_ERROR_MESSAGE_BYTES].decode("utf-8", errors="ignore")

    limit = MAX_ERROR_MESSAGE_BYTES - len(_TRUNCATION_MARKER)
    prefix = encoded[:limit].decode("utf-8", errors="ignore")
    return prefix + _TRUNCATION_MARKER


if __name__ == "__main__":
    raise SystemExit(main())
