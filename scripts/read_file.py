"""Bounded UTF-8 read-file helper for the versioned script protocol.

The helper reads one JSON request from stdin and writes exactly one JSON
response to stdout. Path authorization uses the shared ``path_policy`` module.
Line numbers are 1-based and inclusive. Selected content is capped at
``MAX_CONTENT_BYTES``; when the cap interrupts a selection, the response includes
the next source line to request. A single selected line that exceeds the cap is
returned as a UTF-8-safe prefix with ``partial_line`` set.
"""

from __future__ import annotations

import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, BinaryIO, Iterator

from path_policy import PathPolicy, PathPolicyError

PROTOCOL_VERSION = 1
ACTION = "read_file"
MAX_REQUEST_BYTES = 64 * 1024
MAX_CONTENT_BYTES = 64 * 1024
MAX_ERROR_MESSAGE_BYTES = 1024
MAX_LINE_NUMBER = 0xFFFF_FFFF
_TRUNCATION_MARKER = "...[truncated]"
_ALLOWED_REQUEST_FIELDS = frozenset(
    {"protocol_version", "action", "project_root", "params"}
)
_ALLOWED_PARAM_FIELDS = frozenset({"path", "line_start", "line_end"})


class ReadError(ValueError):
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
            raise ReadError(
                "REQUEST_TOO_LARGE",
                "script request exceeds the configured size bound",
            )
        request = _parse_json_object(raw)
        return _execute(request)
    except ReadError as error:
        return _error_response(error.code, error.detail)
    except PathPolicyError as error:
        return _error_response(error.code, error.detail)
    except (OSError, UnicodeError) as error:
        return _error_response("READ_FAILED", _safe_reason(error))
    except Exception:
        return _error_response(
            "READ_FAILED", "read_file helper failed to process the request"
        )


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReadError("INVALID_REQUEST", "request must be valid JSON") from error
    if not isinstance(value, dict):
        raise ReadError("INVALID_REQUEST", "request must be a JSON object")
    return value


def _execute(request: dict[str, Any]) -> dict[str, Any]:
    unknown = set(request) - _ALLOWED_REQUEST_FIELDS
    if unknown:
        raise ReadError("INVALID_REQUEST", "request contains unknown fields")

    version = request.get("protocol_version")
    if isinstance(version, bool) or not isinstance(version, int):
        raise ReadError("INVALID_REQUEST", "protocol_version must be an integer")
    if version != PROTOCOL_VERSION:
        raise ReadError(
            "UNSUPPORTED_PROTOCOL_VERSION",
            f"unsupported script protocol version: {version}",
        )

    if request.get("action") != ACTION:
        raise ReadError("INVALID_ACTION", "helper only accepts action `read_file`")

    project_root = request.get("project_root")
    if not isinstance(project_root, str) or not project_root:
        raise ReadError("INVALID_REQUEST", "project_root must be a non-empty string")

    params = request.get("params", {})
    if not isinstance(params, dict):
        raise ReadError("INVALID_PARAMS", "params must be a JSON object")
    unknown_params = set(params) - _ALLOWED_PARAM_FIELDS
    if unknown_params:
        raise ReadError("INVALID_PARAMS", "params contains unknown fields")

    raw_path = params.get("path")
    if not isinstance(raw_path, str):
        raise ReadError("INVALID_PARAMS", "path must be a string")

    line_start = _optional_line(params, "line_start")
    line_end = _optional_line(params, "line_end")
    if line_start is not None and line_end is not None and line_end < line_start:
        raise ReadError(
            "INVALID_PARAMS",
            "line_end must be greater than or equal to line_start",
        )

    policy = PathPolicy(project_root)
    relative = policy.authorize_existing_path(raw_path)
    content, first_line, last_line, next_line, truncated, partial_line = (
        _read_selected_lines(policy, relative, line_start, line_end)
    )

    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": True,
        "data": {
            "path": relative.as_posix(),
            "content": content,
            "line_start": first_line,
            "line_end": last_line,
            "next_line": next_line,
            "truncated": truncated,
            "partial_line": partial_line,
        },
    }


def _optional_line(params: dict[str, Any], key: str) -> int | None:
    if key not in params:
        return None
    value = params[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise ReadError("INVALID_PARAMS", f"{key} must be a positive integer")
    if value < 1:
        raise ReadError("INVALID_PARAMS", f"{key} must be greater than zero")
    if value > MAX_LINE_NUMBER:
        raise ReadError(
            "INVALID_PARAMS", f"{key} exceeds the supported line range"
        )
    return value


def _read_selected_lines(
    policy: PathPolicy,
    relative: Path,
    line_start: int | None,
    line_end: int | None,
) -> tuple[str, int | None, int | None, int | None, bool, bool]:
    first_selected = line_start if line_start is not None else 1
    selected: list[str] = []
    selected_bytes = 0
    first_line: int | None = None
    last_line: int | None = None
    line_number = 1

    with _open_regular_file(policy, relative) as handle:
        for raw_line in _iter_lines(handle):
            in_range = line_number >= first_selected and (
                line_end is None or line_number <= line_end
            )
            if in_range:
                remaining = MAX_CONTENT_BYTES - selected_bytes
                if len(raw_line) > MAX_CONTENT_BYTES:
                    text = _decode_prefix(raw_line, remaining) if remaining else ""
                    if text:
                        selected.append(text)
                        if first_line is None:
                            first_line = line_number
                        last_line = line_number
                        return (
                            "".join(selected),
                            first_line,
                            last_line,
                            line_number + 1,
                            True,
                            True,
                        )
                    return (
                        "".join(selected),
                        first_line,
                        last_line,
                        line_number,
                        True,
                        False,
                    )
                if len(raw_line) > remaining:
                    return (
                        "".join(selected),
                        first_line,
                        last_line,
                        line_number,
                        True,
                        False,
                    )
                try:
                    text = raw_line.decode("utf-8", errors="strict")
                except UnicodeDecodeError as error:
                    raise ReadError(
                        "READ_NOT_UTF8", "selected file content is not valid UTF-8 text"
                    ) from error
                selected.append(text)
                selected_bytes += len(raw_line)
                if first_line is None:
                    first_line = line_number
                last_line = line_number

            if line_end is not None and line_number >= line_end:
                break
            line_number += 1

    return "".join(selected), first_line, last_line, None, False, False


def _decode_prefix(raw_line: bytes, limit: int) -> str:
    candidate = raw_line[:limit]
    while candidate:
        try:
            return candidate.decode("utf-8", errors="strict")
        except UnicodeDecodeError as error:
            if error.reason == "unexpected end of data":
                candidate = candidate[: error.start]
                continue
            raise ReadError(
                "READ_NOT_UTF8", "selected file content is not valid UTF-8 text"
            ) from error
    return ""


def _iter_lines(handle: BinaryIO) -> Iterator[bytes]:
    limit = MAX_CONTENT_BYTES + 1
    while True:
        line = handle.readline(limit)
        if not line:
            return
        if line.endswith(b"\n") or len(line) < limit:
            yield line
            continue

        while True:
            chunk = handle.readline(limit)
            if not chunk or chunk.endswith(b"\n"):
                break
        yield line


def _open_regular_file(policy: PathPolicy, relative: Path) -> BinaryIO:
    candidate = policy.root / relative
    try:
        resolved = candidate.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ReadError(
            "READ_UNAVAILABLE", f"file is not accessible: {relative.as_posix()}"
        ) from error

    try:
        resolved_relative = resolved.relative_to(policy.root)
    except ValueError as error:
        raise PathPolicyError(
            "PATH_SYMLINK_ESCAPE",
            f"symlink path is not contained by the project root: {relative.as_posix()}",
        ) from error

    if resolved != policy.root and policy.is_sensitive(resolved_relative):
        raise PathPolicyError(
            "PATH_SENSITIVE", f"sensitive path is denied: {relative.as_posix()}"
        )

    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(resolved, flags)
    except OSError as error:
        raise ReadError(
            "READ_UNAVAILABLE", f"file is not accessible: {relative.as_posix()}"
        ) from error

    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ReadError(
                "READ_NOT_FILE",
                f"path does not name a regular file: {relative.as_posix()}",
            )
    except BaseException:
        os.close(descriptor)
        raise

    return os.fdopen(descriptor, "rb")


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
        return "read_file helper reported an error without a message"

    encoded = sanitized.encode("utf-8")
    if len(encoded) <= MAX_ERROR_MESSAGE_BYTES:
        return sanitized
    if MAX_ERROR_MESSAGE_BYTES <= len(_TRUNCATION_MARKER):
        return encoded[:MAX_ERROR_MESSAGE_BYTES].decode("utf-8", errors="ignore")

    limit = MAX_ERROR_MESSAGE_BYTES - len(_TRUNCATION_MARKER)
    prefix = encoded[:limit].decode("utf-8", errors="ignore")
    return prefix + _TRUNCATION_MARKER


def _safe_reason(error: BaseException) -> str:
    if isinstance(error, OSError) and error.strerror:
        return f"file could not be read: {error.strerror}"
    return "file could not be read"


if __name__ == "__main__":
    raise SystemExit(main())
