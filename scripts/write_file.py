"""Bounded new-file creation helper for the versioned script protocol.

The helper creates exactly one previously nonexistent UTF-8 text file inside
the canonical project root. It validates the target and parent through the
shared ``path_policy`` module, writes a temporary sibling, and publishes the
new file atomically without replacing an existing target. Failures remove the
temporary artifact and any parent directories created for this request.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

from path_policy import PathPolicy, PathPolicyError

PROTOCOL_VERSION = 1
ACTION = "write_file"
MAX_REQUEST_BYTES = 64 * 1024
MAX_CONTENT_BYTES = 48 * 1024
MAX_PATH_BYTES = 256
SHA256_HEX_BYTES = 64
MAX_ERROR_MESSAGE_BYTES = 1024
_TRUNCATION_MARKER = "...[truncated]"
_ALLOWED_REQUEST_FIELDS = frozenset(
    {"protocol_version", "action", "project_root", "params"}
)
_ALLOWED_PARAM_FIELDS = frozenset({"path", "content", "expected_sha256"})
_SHA256_PATTERN = re.compile(r"^[a-f0-9]{64}$")


class WriteError(ValueError):
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
            raise WriteError(
                "REQUEST_TOO_LARGE",
                "script request exceeds the configured size bound",
            )
        request = _parse_json_object(raw)
        return _execute(request)
    except WriteError as error:
        return _error_response(error.code, error.detail)
    except PathPolicyError as error:
        return _error_response(error.code, error.detail)
    except (OSError, UnicodeError) as error:
        return _error_response("WRITE_FAILED", _safe_reason(error))
    except Exception:
        return _error_response(
            "WRITE_FAILED", "write_file helper failed to process the request"
        )


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise WriteError("INVALID_REQUEST", "request must be valid JSON") from error
    if not isinstance(value, dict):
        raise WriteError("INVALID_REQUEST", "request must be a JSON object")
    return value


def _execute(request: dict[str, Any]) -> dict[str, Any]:
    unknown = set(request) - _ALLOWED_REQUEST_FIELDS
    if unknown:
        raise WriteError("INVALID_REQUEST", "request contains unknown fields")

    version = request.get("protocol_version")
    if isinstance(version, bool) or not isinstance(version, int):
        raise WriteError("INVALID_REQUEST", "protocol_version must be an integer")
    if version != PROTOCOL_VERSION:
        raise WriteError(
            "UNSUPPORTED_PROTOCOL_VERSION",
            f"unsupported script protocol version: {version}",
        )

    if request.get("action") != ACTION:
        raise WriteError("INVALID_ACTION", "helper only accepts action `write_file`")

    project_root = request.get("project_root")
    if not isinstance(project_root, str) or not project_root:
        raise WriteError("INVALID_REQUEST", "project_root must be a non-empty string")

    params = request.get("params", {})
    if not isinstance(params, dict):
        raise WriteError("INVALID_PARAMS", "params must be a JSON object")
    unknown_params = set(params) - _ALLOWED_PARAM_FIELDS
    if unknown_params:
        raise WriteError("INVALID_PARAMS", "params contains unknown fields")

    raw_path = params.get("path")
    if not isinstance(raw_path, str):
        raise WriteError("INVALID_PARAMS", "path must be a string")

    content = params.get("content")
    if not isinstance(content, str):
        raise WriteError("INVALID_PARAMS", "content must be a string")

    expected_sha256 = params.get("expected_sha256")
    if expected_sha256 is not None:
        if not isinstance(expected_sha256, str):
            raise WriteError("INVALID_PARAMS", "expected_sha256 must be a string")
        if not _SHA256_PATTERN.fullmatch(expected_sha256):
            raise WriteError(
                "INVALID_PARAMS",
                "expected_sha256 must be exactly 64 lowercase hexadecimal characters",
            )

    _validate_path_text(raw_path)
    payload = _encode_content(content)
    if b"\x00" in payload:
        raise WriteError(
            "WRITE_CONTENT_NUL", "content must be UTF-8 text without NUL bytes"
        )

    policy = PathPolicy(project_root)
    relative = policy.authorize_new_file_path(raw_path)
    target = policy.root / relative
    if os.path.lexists(target):
        raise WriteError(
            "WRITE_TARGET_EXISTS",
            f"target file already exists: {relative.as_posix()}",
        )

    created_directories: list[Path] = []
    temporary: Path | None = None
    published = False
    try:
        created_directories = _ensure_parent(policy, relative)
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=f".{target.name}.rupi-",
            suffix=".tmp",
            dir=target.parent,
        )
        temporary = Path(temporary_name)
        try:
            handle = os.fdopen(descriptor, "wb")
        except BaseException:
            os.close(descriptor)
            raise
        with handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())

        _publish_new_file(temporary, target)
        temporary = None
        published = True
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if not published:
            _cleanup_directories(created_directories)

    digest = hashlib.sha256(payload).hexdigest()
    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": True,
        "data": {
            "path": relative.as_posix(),
            "created": True,
            "bytes_written": len(payload),
            "sha256": digest,
        },
    }


def _validate_path_text(raw: str) -> None:
    if not raw.strip():
        raise WriteError("PATH_EMPTY", "path must not be empty")
    if len(raw.encode("utf-8", errors="surrogatepass")) > MAX_PATH_BYTES:
        raise WriteError(
            "WRITE_PATH_TOO_LONG",
            f"path exceeds the {MAX_PATH_BYTES}-byte bound",
        )
    if "\x00" in raw:
        raise WriteError("PATH_NUL", "path must not contain NUL bytes")
    if any(ord(character) < 0x20 or 0x7F <= ord(character) <= 0x9F for character in raw):
        raise WriteError("WRITE_PATH_CONTROL", "path must not contain control characters")


def _encode_content(content: str) -> bytes:
    try:
        payload = content.encode("utf-8")
    except UnicodeEncodeError as error:
        raise WriteError(
            "WRITE_NOT_UTF8", "content must be valid UTF-8 text"
        ) from error
    if len(payload) > MAX_CONTENT_BYTES:
        raise WriteError(
            "WRITE_CONTENT_TOO_LARGE",
            f"content exceeds the {MAX_CONTENT_BYTES}-byte bound",
        )
    return payload


def _ensure_parent(policy: PathPolicy, relative: Path) -> list[Path]:
    parent = relative.parent
    if parent == Path(".") or not parent.parts:
        return []

    parent_path = policy.root / parent
    missing: list[Path] = []
    probe = parent_path
    while not os.path.lexists(probe):
        missing.append(probe)
        next_probe = probe.parent
        if next_probe == probe:
            break
        probe = next_probe

    if not probe.is_dir():
        raise WriteError(
            "WRITE_PARENT_NOT_DIRECTORY",
            f"parent path is not a directory: {parent.as_posix()}",
        )

    created: list[Path] = []
    try:
        for directory in reversed(missing):
            try:
                directory.mkdir()
            except FileExistsError:
                if not directory.is_dir():
                    raise WriteError(
                        "WRITE_PARENT_NOT_DIRECTORY",
                        f"parent path is not a directory: {parent.as_posix()}",
                    )
            else:
                created.append(directory)
    except BaseException:
        _cleanup_directories(created)
        raise

    resolved = parent_path.resolve(strict=True)
    try:
        resolved.relative_to(policy.root)
    except ValueError as error:
        _cleanup_directories(created)
        raise PathPolicyError(
            "PATH_SYMLINK_ESCAPE",
            f"parent path is not contained by the project root: {parent.as_posix()}",
        ) from error

    if not resolved.is_dir():
        _cleanup_directories(created)
        raise WriteError(
            "WRITE_PARENT_NOT_DIRECTORY",
            f"parent path is not a directory: {parent.as_posix()}",
        )
    return created


def _publish_new_file(temporary: Path, target: Path) -> None:
    try:
        os.link(temporary, target, follow_symlinks=False)
    except FileExistsError as error:
        raise WriteError(
            "WRITE_TARGET_EXISTS",
            f"target file already exists: {target.name}",
        ) from error
    except OSError as error:
        raise WriteError(
            "WRITE_ATOMIC_PUBLISH_FAILED",
            _safe_reason(error),
        ) from error

    try:
        temporary.unlink()
    except FileNotFoundError:
        pass
    except OSError as error:
        try:
            target.unlink()
        except OSError:
            pass
        raise WriteError(
            "WRITE_TEMP_CLEANUP_FAILED",
            "temporary file could not be removed after publishing",
        ) from error


def _cleanup_directories(created: list[Path]) -> None:
    for directory in reversed(created):
        try:
            directory.rmdir()
        except OSError:
            return


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
        return "write_file helper reported an error without a message"

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
        return f"file could not be written: {error.strerror}"
    return "file could not be written"


if __name__ == "__main__":
    raise SystemExit(main())
