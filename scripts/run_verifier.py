"""Bounded argv-only verifier helper for the versioned script protocol.

The helper executes exactly one harness-selected verification command. It never
uses a shell, never installs dependencies, bounds argv, timeout, cwd, and
captured output, and returns structured duration/status evidence.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

PROTOCOL_VERSION = 1
ACTION = "run_verifier"
MAX_REQUEST_BYTES = 64 * 1024
MAX_PROGRAM_BYTES = 1024
MAX_ARG_COUNT = 128
MAX_ARG_BYTES = 4096
MAX_ARGS_BYTES = 32 * 1024
MAX_CWD_BYTES = 256
MAX_TIMEOUT_MS = 900_000
MAX_OUTPUT_CAP = 64 * 1024
MAX_ERROR_MESSAGE_BYTES = 1024
_READ_CHUNK_BYTES = 8192
_TRUNCATION_MARKER = "...[truncated]"
_OUTPUT_MARKER = "...[{} bytes omitted]..."
_ALLOWED_REQUEST_FIELDS = frozenset(
    {"protocol_version", "action", "project_root", "params"}
)
_ALLOWED_PARAM_FIELDS = frozenset(
    {"program", "args", "cwd", "timeout_ms", "output_cap"}
)
_FORBIDDEN_PROGRAMS = frozenset(
    {
        "sh",
        "bash",
        "dash",
        "zsh",
        "ksh",
        "csh",
        "tcsh",
        "fish",
        "cmd",
        "powershell",
        "pwsh",
        "sudo",
        "su",
        "doas",
        "env",
    }
)


class VerifierError(ValueError):
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
            raise VerifierError(
                "REQUEST_TOO_LARGE",
                "script request exceeds the configured size bound",
            )
        request = _parse_json_object(raw)
        return _execute(request)
    except VerifierError as error:
        return _error_response(error.code, error.detail)
    except (OSError, UnicodeError) as error:
        return _error_response("VERIFIER_FAILED", _safe_reason(error))
    except Exception:
        return _error_response(
            "VERIFIER_FAILED", "run_verifier helper failed to process the request"
        )


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise VerifierError("INVALID_REQUEST", "request must be valid JSON") from error
    if not isinstance(value, dict):
        raise VerifierError("INVALID_REQUEST", "request must be a JSON object")
    return value


def _execute(request: dict[str, Any]) -> dict[str, Any]:
    unknown = set(request) - _ALLOWED_REQUEST_FIELDS
    if unknown:
        raise VerifierError("INVALID_REQUEST", "request contains unknown fields")

    version = request.get("protocol_version")
    if isinstance(version, bool) or not isinstance(version, int):
        raise VerifierError("INVALID_REQUEST", "protocol_version must be an integer")
    if version != PROTOCOL_VERSION:
        raise VerifierError(
            "UNSUPPORTED_PROTOCOL_VERSION",
            f"unsupported script protocol version: {version}",
        )

    if request.get("action") != ACTION:
        raise VerifierError("INVALID_ACTION", "helper only accepts action `run_verifier`")

    project_root = request.get("project_root")
    if not isinstance(project_root, str) or not project_root:
        raise VerifierError("INVALID_REQUEST", "project_root must be a non-empty string")

    params = request.get("params", {})
    if not isinstance(params, dict):
        raise VerifierError("INVALID_PARAMS", "params must be a JSON object")
    unknown_params = set(params) - _ALLOWED_PARAM_FIELDS
    if unknown_params:
        raise VerifierError("INVALID_PARAMS", "params contains unknown fields")

    program = _program(params.get("program"))
    args = _args(params.get("args"))
    root = _project_root(project_root)
    cwd = _working_directory(root, params.get("cwd"))
    timeout_ms = _bounded_integer(
        params.get("timeout_ms"),
        "timeout_ms",
        maximum=MAX_TIMEOUT_MS,
    )
    output_cap = _bounded_integer(
        params.get("output_cap"),
        "output_cap",
        maximum=MAX_OUTPUT_CAP,
    )

    return _run(program, args, cwd, timeout_ms, output_cap)


def _program(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise VerifierError("INVALID_PARAMS", "program must be a non-empty string")
    if len(value.encode("utf-8", errors="surrogatepass")) > MAX_PROGRAM_BYTES:
        raise VerifierError(
            "VERIFIER_PROGRAM_TOO_LONG",
            f"program exceeds the {MAX_PROGRAM_BYTES}-byte bound",
        )
    if "\x00" in value:
        raise VerifierError("INVALID_PARAMS", "program must not contain NUL bytes")
    if _has_control(value):
        raise VerifierError("INVALID_PARAMS", "program must not contain control characters")
    name = Path(value).name.lower()
    if name.endswith(".exe"):
        name = name[:-4]
    if name in _FORBIDDEN_PROGRAMS:
        raise VerifierError(
            "VERIFIER_PROGRAM_FORBIDDEN",
            f"program is not allowed for verification: {name}",
        )
    return value


def _args(value: Any) -> list[str]:
    if not isinstance(value, list):
        raise VerifierError("INVALID_PARAMS", "args must be an array of strings")
    if len(value) > MAX_ARG_COUNT:
        raise VerifierError(
            "VERIFIER_ARGS_TOO_LARGE",
            f"args exceed the {MAX_ARG_COUNT}-element bound",
        )
    total = 0
    args: list[str] = []
    for arg in value:
        if not isinstance(arg, str):
            raise VerifierError("INVALID_PARAMS", "args must be an array of strings")
        encoded = arg.encode("utf-8", errors="surrogatepass")
        if len(encoded) > MAX_ARG_BYTES:
            raise VerifierError(
                "VERIFIER_ARGS_TOO_LARGE",
                f"one argument exceeds the {MAX_ARG_BYTES}-byte bound",
            )
        total += len(encoded)
        if total > MAX_ARGS_BYTES:
            raise VerifierError(
                "VERIFIER_ARGS_TOO_LARGE",
                f"arguments exceed the {MAX_ARGS_BYTES}-byte total bound",
            )
        if "\x00" in arg:
            raise VerifierError("INVALID_PARAMS", "args must not contain NUL bytes")
        args.append(arg)
    return args


def _project_root(raw: str) -> Path:
    try:
        root = Path(raw).resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise VerifierError(
            "VERIFIER_ROOT_INACCESSIBLE", "project root is not accessible"
        ) from error
    if not root.is_dir():
        raise VerifierError(
            "VERIFIER_ROOT_INACCESSIBLE", "project root must be a directory"
        )
    return root


def _working_directory(root: Path, value: Any) -> Path:
    if not isinstance(value, str) or not value.strip():
        raise VerifierError("INVALID_PARAMS", "cwd must be a non-empty string")
    if len(value.encode("utf-8", errors="surrogatepass")) > MAX_CWD_BYTES:
        raise VerifierError(
            "VERIFIER_CWD_TOO_LONG", f"cwd exceeds the {MAX_CWD_BYTES}-byte bound"
        )
    if "\x00" in value:
        raise VerifierError("INVALID_PARAMS", "cwd must not contain NUL bytes")
    if _has_control(value):
        raise VerifierError("INVALID_PARAMS", "cwd must not contain control characters")

    raw = Path(value)
    candidate = raw if raw.is_absolute() else root / raw
    try:
        resolved = candidate.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise VerifierError(
            "VERIFIER_CWD_INACCESSIBLE", "verification cwd is not accessible"
        ) from error
    if not resolved.is_dir():
        raise VerifierError(
            "VERIFIER_CWD_INACCESSIBLE", "verification cwd must be a directory"
        )
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise VerifierError(
            "VERIFIER_CWD_ESCAPE",
            "verification cwd escapes the project root",
        ) from error
    return resolved


def _has_control(value: str) -> bool:
    return any(
        ord(character) < 0x20 or 0x7F <= ord(character) <= 0x9F
        for character in value
    )


def _bounded_integer(value: Any, name: str, *, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise VerifierError("INVALID_PARAMS", f"{name} must be an integer")
    if value <= 0 or value > maximum:
        raise VerifierError(
            "INVALID_PARAMS", f"{name} must be between 1 and {maximum}"
        )
    return value


def _run(
    program: str,
    args: list[str],
    cwd: Path,
    timeout_ms: int,
    output_cap: int,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            [program, *args],
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            close_fds=True,
            start_new_session=True,
            preexec_fn=_child_preexec() if os.name == "posix" else None,
        )
    except OSError as error:
        raise VerifierError(
            "VERIFIER_START_FAILED",
            f"verification command could not be started: {_safe_reason(error)}",
        ) from error

    assert process.stdout is not None
    assert process.stderr is not None
    stdout_capture: list[_BoundedCapture | None] = [None]
    stderr_capture: list[_BoundedCapture | None] = [None]
    errors: list[BaseException] = []
    stdout_thread = threading.Thread(
        target=_capture_stream,
        args=(process.stdout, output_cap, stdout_capture, errors),
        daemon=True,
    )
    stderr_thread = threading.Thread(
        target=_capture_stream,
        args=(process.stderr, output_cap, stderr_capture, errors),
        daemon=True,
    )
    stdout_thread.start()
    stderr_thread.start()

    timed_out = False
    try:
        process.wait(timeout=timeout_ms / 1000)
    except subprocess.TimeoutExpired:
        timed_out = True
        _terminate(process)
    except BaseException:
        _terminate(process)
        raise
    finally:
        stdout_thread.join(timeout=5)
        stderr_thread.join(timeout=5)
        if stdout_thread.is_alive() or stderr_thread.is_alive():
            _terminate(process)
            stdout_thread.join()
            stderr_thread.join()

    if errors:
        error = errors[0]
        raise VerifierError("VERIFIER_OUTPUT_FAILED", _safe_reason(error)) from error

    duration_ms = int((time.monotonic() - started) * 1000)
    stdout = stdout_capture[0] or _BoundedCapture(output_cap)
    stderr = stderr_capture[0] or _BoundedCapture(output_cap)
    return {
        "protocol_version": PROTOCOL_VERSION,
        "ok": True,
        "data": {
            "exit_code": _exit_code(process, timed_out),
            "timed_out": timed_out,
            "duration_ms": duration_ms,
            "stdout": stdout.finish(),
            "stderr": stderr.finish(),
        },
    }


def _child_preexec():
    def setup() -> None:
        if not sys.platform.startswith("linux"):
            return
        try:
            import ctypes

            libc = ctypes.CDLL(None, use_errno=True)
            libc.prctl(1, signal.SIGKILL)
        except Exception:
            return

    return setup


def _terminate(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "posix":
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, OSError):
            pass
        else:
            try:
                process.wait(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass
            return
    try:
        process.kill()
    except OSError:
        pass
    try:
        process.wait(timeout=5)
    except (subprocess.TimeoutExpired, OSError):
        pass


def _exit_code(process: subprocess.Popen[bytes], timed_out: bool) -> int:
    if timed_out:
        return -2
    code = process.returncode
    if code is None or code < 0:
        return -1
    return code


def _capture_stream(
    stream: Any,
    cap: int,
    holder: list[_BoundedCapture | None],
    errors: list[BaseException],
) -> None:
    try:
        capture = _BoundedCapture(cap)
        while True:
            chunk = stream.read(_READ_CHUNK_BYTES)
            if not chunk:
                break
            capture.push(chunk)
        holder[0] = capture
    except BaseException as error:
        errors.append(error)
    finally:
        try:
            stream.close()
        except OSError:
            pass


class _BoundedCapture:
    def __init__(self, cap: int) -> None:
        self.cap = cap
        self.head = bytearray()
        self.tail = bytearray()
        self.total = 0

    def push(self, chunk: bytes) -> None:
        self.total += len(chunk)
        head_cap = self.cap // 2
        if len(self.head) < head_cap:
            take = min(head_cap - len(self.head), len(chunk))
            self.head.extend(chunk[:take])
            chunk = chunk[take:]
        tail_cap = self.cap - head_cap
        if chunk and tail_cap > 0:
            self.tail.extend(chunk)
            if len(self.tail) > tail_cap:
                del self.tail[: len(self.tail) - tail_cap]

    def finish(self) -> str:
        omitted = self.total - (len(self.head) + len(self.tail))
        head = self.head.decode("utf-8", errors="replace")
        tail = self.tail.decode("utf-8", errors="replace")
        if omitted <= 0:
            return head + tail
        return f"{head}\n{_OUTPUT_MARKER.format(omitted)}\n{tail}"


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
        return "run_verifier helper reported an error without a message"

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
        return f"verification command failed: {error.strerror}"
    return "verification command failed"


if __name__ == "__main__":
    raise SystemExit(main())
