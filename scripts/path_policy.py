"""Shared project-root and sensitive-path contract for fixed helper scripts.

The Rust orchestrator uses ``src/path_policy.rs``; helper scripts use this
module. Both enforce the same repository-relative rules and stable ``PATH_*``
error codes. Existing targets and the nearest existing ancestor of proposed new
files are resolved through symlinks; escapes and unresolvable links fail closed.
"""

from __future__ import annotations

from pathlib import Path
from typing import Iterable

MAX_PATH_ERROR_BYTES = 256

DEFAULT_SENSITIVE_NAMES = frozenset(
    {
        ".env",
        ".envrc",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".git-credentials",
        "credentials",
        "credentials.json",
        "secrets",
        "secrets.json",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "authorized_keys",
        "known_hosts",
    }
)

DEFAULT_SENSITIVE_DIRECTORIES = frozenset(
    {
        ".ssh",
        ".aws",
        ".gcp",
        ".gnupg",
        ".docker",
        ".kube",
        ".azure",
        ".password-store",
        ".config",
        ".harness-audit",
        ".ai-wiki",
    }
)

DEFAULT_SENSITIVE_EXTENSIONS = frozenset({"pem", "key", "p12", "pfx", "jks", "kdbx"})

_TRUNCATION_MARKER = "...[truncated]"


class PathPolicyError(ValueError):
    """Bounded path-policy denial with a stable machine-readable code."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        self.detail = message
        super().__init__(f"[{code}] {message}")


class PathPolicy:
    """Canonical project root plus deny-by-default sensitive patterns."""

    def __init__(
        self,
        root: str | Path,
        sensitive_names: Iterable[str] = DEFAULT_SENSITIVE_NAMES,
        sensitive_directories: Iterable[str] = DEFAULT_SENSITIVE_DIRECTORIES,
        sensitive_extensions: Iterable[str] = DEFAULT_SENSITIVE_EXTENSIONS,
    ) -> None:
        requested = Path(root)
        raw_root = _safe_path(str(requested))
        try:
            canonical_root = requested.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise PathPolicyError(
                "PATH_ROOT_INACCESSIBLE",
                f"project root is not accessible: {raw_root}",
            ) from error
        if not canonical_root.is_dir():
            raise PathPolicyError(
                "PATH_ROOT_NOT_DIRECTORY",
                f"project root must be a directory: {raw_root}",
            )

        self.root = canonical_root
        self.sensitive_names = _lowercase_set(sensitive_names)
        self.sensitive_directories = _lowercase_set(sensitive_directories)
        self.sensitive_extensions = _lowercase_set(sensitive_extensions)

    def authorize_path(self, raw: str) -> Path:
        """Validate a repository-relative path with no implied existence."""
        if not isinstance(raw, str):
            raise PathPolicyError("PATH_EMPTY", "path must not be empty")
        if not raw.strip():
            raise PathPolicyError("PATH_EMPTY", "path must not be empty")
        if "\x00" in raw:
            raise PathPolicyError("PATH_NUL", "path must not contain NUL bytes")

        requested = _safe_path(raw)
        path = Path(raw)
        if path.is_absolute():
            raise PathPolicyError(
                "PATH_ABSOLUTE", f"absolute paths are not allowed: {requested}"
            )

        normalized: list[str] = []
        for part in path.parts:
            if part in ("", "."):
                continue
            if part == "..":
                if not normalized:
                    raise PathPolicyError(
                        "PATH_TRAVERSAL",
                        f"path escapes the project root: {requested}",
                    )
                normalized.pop()
                continue
            if _is_drive_or_root_component(part):
                raise PathPolicyError(
                    "PATH_ABSOLUTE", f"absolute paths are not allowed: {requested}"
                )
            normalized.append(part)

        if not normalized:
            raise PathPolicyError("PATH_EMPTY", "path must not be empty")

        relative = Path(*normalized)
        if self.is_sensitive(relative):
            raise PathPolicyError(
                "PATH_SENSITIVE", f"sensitive path is denied: {requested}"
            )

        self._reject_symlink_escape(relative, requested)
        return relative

    def authorize_existing_path(self, raw: str) -> Path:
        """Validate a path that should identify an existing target.

        Existing targets and the nearest existing ancestor are resolved through
        symlinks. Existence and target-type checks remain with the caller.
        """
        return self.authorize_path(raw)

    def authorize_new_file_path(self, raw: str) -> Path:
        """Validate a proposed new-file path whose final component may not exist.

        The nearest existing ancestor, including symlinked parent directories,
        must resolve inside the canonical root and outside sensitive targets.
        """
        return self.authorize_path(raw)

    def is_sensitive(self, path: Path) -> bool:
        """Return whether a normalized relative path matches a denied pattern."""
        for part in path.parts:
            name = part.lower()
            if (
                name in self.sensitive_directories
                or name in self.sensitive_names
                or name.startswith(".env.")
                or name.startswith("credentials.")
                or name.startswith("secrets.")
            ):
                return True

        extension = path.suffix.lower().lstrip(".")
        return bool(extension) and extension in self.sensitive_extensions

    def _reject_symlink_escape(self, relative: Path, requested: str) -> None:
        candidate = self.root / relative
        existing = candidate
        while True:
            try:
                existing.lstat()
                break
            except FileNotFoundError:
                if existing == self.root:
                    return
                parent = existing.parent
                if parent == existing:
                    return
                existing = parent
            except OSError as error:
                raise PathPolicyError(
                    "PATH_SYMLINK_ESCAPE",
                    f"symlink path is not contained by the project root: {requested}",
                ) from error

        try:
            resolved = existing.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise PathPolicyError(
                "PATH_SYMLINK_ESCAPE",
                f"symlink path is not contained by the project root: {requested}",
            ) from error

        try:
            resolved_relative = resolved.relative_to(self.root)
        except ValueError as error:
            raise PathPolicyError(
                "PATH_SYMLINK_ESCAPE",
                f"symlink path is not contained by the project root: {requested}",
            ) from error

        if resolved != self.root and self.is_sensitive(resolved_relative):
            raise PathPolicyError(
                "PATH_SENSITIVE", f"sensitive path is denied: {requested}"
            )


def _is_drive_or_root_component(part: str) -> bool:
    return len(part) == 2 and part[1] == ":" and part[0].isalpha()


def _lowercase_set(values: Iterable[str]) -> frozenset[str]:
    return frozenset(str(value).lower() for value in values)


def _safe_path(raw: str) -> str:
    sanitized = "".join(
        character if character.isprintable() else "\ufffd" for character in raw
    )
    encoded = sanitized.encode("utf-8")
    if len(encoded) <= MAX_PATH_ERROR_BYTES:
        return sanitized

    marker = _TRUNCATION_MARKER.encode("utf-8")
    if MAX_PATH_ERROR_BYTES <= len(marker):
        return encoded[:MAX_PATH_ERROR_BYTES].decode("utf-8", errors="ignore")
    limit = MAX_PATH_ERROR_BYTES - len(marker)
    prefix = encoded[:limit].decode("utf-8", errors="ignore")
    return prefix + _TRUNCATION_MARKER
