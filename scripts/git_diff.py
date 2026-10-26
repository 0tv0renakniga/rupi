"""Bounded read-only Git diff helper for the versioned script protocol.

The helper reads one JSON request from stdin and writes exactly one JSON
response to stdout. It runs only ``git diff`` or ``git diff --cached`` in the
configured root with a minimal environment, no pager, and optional Git locks
disabled. Diff output is bounded, made display-safe, and preserves both a head
and a tail when truncated.
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
ACTION = "git_diff"
MAX_REQUEST_BYTES = 64 * 1024
MAX_DIFF_BYTES = 64 * 1024
MAX_STDERR_BYTES = 8 * 1024
MAX_ERROR_MESSAGE_BYTES = 1024
GIT_TIMEOUT_SECONDS = 30
_HEAD_BYTES = 48 * 1024
_TRUNCATION_MARKER = "\n... [diff truncated] ...\n"
_ERROR_TRUNCATION_MARKER = "...[truncated]"
_ALLOWED_REQUEST_FIELDS = frozenset(
    {"protocol_version", "action", "project_root", "params"}
)
_ALLOWED_PARAM_FIELDS = frozenset({"staged"})
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


class DiffError(ValueError):
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
            raise DiffError(
                "REQUEST_TOO_LARGE",
                "script request exceeds the configured size bound",
            )
        request = _parse_json_object(raw)
        return _execute(request)
    except DiffError as error:
        return _error_response(error.code, error.detail)
    except PathPolicyError as error:
        return _error_response(error.code, error.detail)
    except Exception:
        return _error_response(
            "GIT_DIFF_FAILED", "git diff helper failed to process the request"
        )


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise DiffError("INVALID_REQUEST", "request must be valid JSON") from error
    if not isinstance(value, dict):
        raise DiffError("INVALID_REQUEST", "request must be a JSON object")
    return value


def _execute(request: dict[str, Any]) -> dict[str, Any]:
    unknown = set(request) - _ALLOWED_REQUEST_FIELDS
    if unknown:
        raise DiffError("INVALID_REQUEST", "request contains unknown fields")

    version = request.get("protocol_version")
    if isinstance(version, bool) or not isinstance(version, int):
        raise DiffError("INVALID_REQUEST", "protocol_version must be an integer")
    if version != PROTOCOL_VERSION:
        raise DiffError(
            "UNSUPPORTED_PROTOCOL_VERSION",
            f"unsupported script protocol version: {version}",
        )

    if request.get("action") != ACTION:
        raise DiffError("INVALID_ACTION", "helper only accepts action `git_diff`")

    project_root = request.get("project_root")
    if not isinstance(project_root, str) or not project_root:
        raise DiffError("INVALID_REQUEST", "project_root must be a non-empty string")

    params = request.get("params", {})
    if not isinstance(params, dict):
        raise DiffError("INVALID_PARAMS", "params must be a JSON object")
    unknown_params = set(params) - _ALLOWED_PARAM_FIELDS
    if unknown_params:
        raise DiffError("INVALID_PARAMS", "params contains unknown fields")
    if "staged" not in params:
        raise DiffError("INVALID_PARAMS", "staged is required and must be a boolean")
    staged = params["staged"]
    if not isinstance(staged, bool):
        raise DiffError("INVALID_PARAMS", "staged must be a boolean")

    policy = PathPolicy(project_root)
    diff, truncated = _run_git_diff(policy.root, staged)

    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": True,
        "data": {
            "staged": staged,
            "diff": diff,
            "truncated": truncated,
        },
    }


def _run_git_diff(root: Path, staged: bool) -> tuple[str, bool]:
    environment = _git_environment()
    git = shutil.which("git", path=environment["PATH"])
    if git is None:
        raise DiffError("GIT_UNAVAILABLE", "git executable is unavailable")

    args = [
        git,
        "--no-pager",
        "-c",
        "core.quotePath=true",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "color.ui=false",
        "diff",
    ]
    if staged:
        args.append("--cached")
    args.extend(
        [
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            ".",
        ]
    )

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
        raise DiffError("GIT_UNAVAILABLE", "git could not be started") from error

    diff_result: dict[str, tuple[bytes, bool]] = {}
    stderr_result: dict[str, tuple[bytes, bool]] = {}
    read_errors: list[OSError] = []
    threads = [
        threading.Thread(
            target=_collect_diff,
            args=(process.stdout, diff_result, read_errors),
            daemon=True,
        ),
        threading.Thread(
            target=_collect_stream,
            args=("stderr", process.stderr, MAX_STDERR_BYTES, stderr_result, read_errors),
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
        raise DiffError(
            "GIT_TIMEOUT", "git diff exceeded the configured timeout"
        ) from error

    if not _join_threads(threads):
        _terminate(process)
        raise DiffError("GIT_DIFF_FAILED", "git diff output could not be collected")
    if read_errors:
        raise DiffError("GIT_DIFF_FAILED", "git diff output could not be collected")

    stdout, stdout_truncated = diff_result.get("stdout", (b"", False))
    stderr, stderr_truncated = stderr_result.get("stderr", (b"", False))
    if process.returncode != 0:
        diagnostic = _diagnostic(stderr, stderr_truncated)
        if "not a git repository" in diagnostic.lower():
            raise DiffError(
                "NOT_A_GIT_REPOSITORY",
                "configured root is not a Git repository",
            )
        message = diagnostic or f"git diff exited with status {process.returncode}"
        raise DiffError("GIT_DIFF_FAILED", message)

    return _bounded_diff(stdout, stdout_truncated)


def _collect_diff(
    stream: BinaryIO | None,
    results: dict[str, tuple[bytes, bool]],
    errors: list[OSError],
) -> None:
    if stream is None:
        results["stdout"] = (b"", False)
        return
    try:
        results["stdout"] = _read_head_and_tail(stream)
    except OSError as error:
        errors.append(error)


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


def _read_head_and_tail(stream: BinaryIO) -> tuple[bytes, bool]:
    head = bytearray()
    tail = bytearray()
    total = 0
    marker = _TRUNCATION_MARKER.encode("utf-8")

    while True:
        chunk = stream.read(8192)
        if not chunk:
            break
        total += len(chunk)
        if len(head) < _HEAD_BYTES:
            take = min(_HEAD_BYTES - len(head), len(chunk))
            head.extend(chunk[:take])
        tail.extend(chunk)
        if len(tail) > MAX_DIFF_BYTES:
            del tail[: len(tail) - MAX_DIFF_BYTES]

    if total <= MAX_DIFF_BYTES:
        if total <= len(head):
            return bytes(head[:total]), False
        remaining = total - len(head)
        return bytes(head) + bytes(tail[-remaining:]), False

    tail_limit = MAX_DIFF_BYTES - len(head) - len(marker)
    if tail_limit <= 0:
        return bytes(head) + marker, True
    return bytes(head) + marker + bytes(tail[-tail_limit:]), True


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


def _bounded_diff(raw: bytes, truncated: bool) -> tuple[str, bool]:
    text = raw.decode("utf-8", errors="replace")
    safe = "".join(
        character
        if character in "\n\t" or character.isprintable()
        else " "
        for character in text
    )
    encoded = safe.encode("utf-8")
    if len(encoded) <= MAX_DIFF_BYTES:
        return safe, truncated

    marker = _TRUNCATION_MARKER
    tail_limit = MAX_DIFF_BYTES - _HEAD_BYTES - len(marker.encode("utf-8"))
    head = encoded[:_HEAD_BYTES].decode("utf-8", errors="ignore")
    if tail_limit <= 0:
        return head + marker, True
    tail = encoded[-tail_limit:].decode("utf-8", errors="ignore")
    return head + marker + tail, True


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
        return "git diff helper reported an error without a message"

    encoded = sanitized.encode("utf-8")
    if len(encoded) <= MAX_ERROR_MESSAGE_BYTES:
        return sanitized
    if MAX_ERROR_MESSAGE_BYTES <= len(_ERROR_TRUNCATION_MARKER):
        return encoded[:MAX_ERROR_MESSAGE_BYTES].decode("utf-8", errors="ignore")

    limit = MAX_ERROR_MESSAGE_BYTES - len(_ERROR_TRUNCATION_MARKER)
    prefix = encoded[:limit].decode("utf-8", errors="ignore")
    return prefix + _ERROR_TRUNCATION_MARKER


if __name__ == "__main__":
    raise SystemExit(main())
