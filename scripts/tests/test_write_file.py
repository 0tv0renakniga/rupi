import hashlib
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import write_file  # noqa: E402
from write_file import MAX_CONTENT_BYTES, MAX_PATH_BYTES  # noqa: E402


class WriteFileScriptTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-write-file-")
        self.root = Path(self._temp.name)

    def tearDown(self):
        self._temp.cleanup()

    def request(self, **params):
        return {
            "protocol_version": 1,
            "action": "write_file",
            "project_root": str(self.root),
            "params": params,
        }

    def run_raw(self, raw):
        completed = subprocess.run(
            [sys.executable, str(SCRIPTS / "write_file.py")],
            input=raw,
            cwd=self.root,
            capture_output=True,
            timeout=15,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        self.assertEqual(completed.stderr, b"")
        return json.loads(completed.stdout)

    def execute(self, **params):
        return self.run_raw(json.dumps(self.request(**params)).encode("utf-8"))

    def assert_error(self, response, code):
        self.assertEqual(response["protocol_version"], 1)
        self.assertFalse(response["ok"])
        self.assertNotIn("data", response)
        self.assertEqual(response["error"]["code"], code)
        self.assertTrue(response["error"]["message"])
        self.assertLessEqual(len(response["error"]["message"].encode("utf-8")), 1024)

    def assert_success(self, response):
        self.assertEqual(response["protocol_version"], 1)
        self.assertTrue(response["ok"])
        self.assertNotIn("error", response)
        return response["data"]

    def symlink(self, target, link):
        try:
            os.symlink(target, link)
        except (OSError, NotImplementedError):
            self.skipTest("symlink creation is unavailable")

    def assert_no_temporary_files(self, directory):
        if not directory.exists():
            return
        self.assertEqual(
            [path.name for path in directory.iterdir() if path.name.endswith(".tmp")],
            [],
        )

    def test_creates_one_new_utf8_file_with_digest_metadata(self):
        content = "hello\nsnowman: \u2603\n"
        data = self.assert_success(
            self.execute(path="nested/deep/new.txt", content=content)
        )

        target = self.root / "nested/deep/new.txt"
        self.assertEqual(target.read_text(encoding="utf-8"), content)
        self.assertEqual(
            data,
            {
                "path": "nested/deep/new.txt",
                "created": True,
                "bytes_written": len(content.encode("utf-8")),
                "sha256": hashlib.sha256(content.encode("utf-8")).hexdigest(),
            },
        )
        self.assert_no_temporary_files(target.parent)

    def test_accepts_empty_content_and_an_optional_valid_digest(self):
        data = self.assert_success(
            self.execute(
                path="empty.txt",
                content="",
                expected_sha256="a" * 64,
            )
        )
        self.assertEqual(data["bytes_written"], 0)
        self.assertEqual(
            data["sha256"], hashlib.sha256(b"").hexdigest()
        )
        self.assertEqual((self.root / "empty.txt").read_bytes(), b"")

    def test_existing_target_requires_an_expected_digest(self):
        target = self.root / "existing.txt"
        target.write_text("original\n", encoding="utf-8")
        self.assert_error(
            self.execute(path="existing.txt", content="replacement\n"),
            "WRITE_HASH_REQUIRED",
        )
        self.assertEqual(target.read_text(encoding="utf-8"), "original\n")
        self.assert_no_temporary_files(self.root)

        (self.root / "directory").mkdir()
        self.assert_error(
            self.execute(
                path="directory",
                content="replacement\n",
                expected_sha256=hashlib.sha256(b"original\n").hexdigest(),
            ),
            "WRITE_TARGET_NOT_FILE",
        )

        inside = self.root / "inside.txt"
        inside.write_text("inside\n", encoding="utf-8")
        self.symlink(inside, self.root / "inside-link")
        self.assert_error(
            self.execute(
                path="inside-link",
                content="replacement\n",
                expected_sha256=hashlib.sha256(b"inside\n").hexdigest(),
            ),
            "WRITE_TARGET_NOT_FILE",
        )
        self.assertEqual(inside.read_text(encoding="utf-8"), "inside\n")

    def test_matching_digest_replaces_file_and_preserves_mode(self):
        target = self.root / "existing.txt"
        original = "original\n"
        replacement = "replacement\n"
        target.write_text(original, encoding="utf-8")
        os.chmod(target, 0o640)

        data = self.assert_success(
            self.execute(
                path="existing.txt",
                content=replacement,
                expected_sha256=hashlib.sha256(original.encode("utf-8")).hexdigest(),
            )
        )

        self.assertEqual(target.read_text(encoding="utf-8"), replacement)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o640)
        self.assertEqual(
            data,
            {
                "path": "existing.txt",
                "created": False,
                "bytes_written": len(replacement.encode("utf-8")),
                "sha256": hashlib.sha256(replacement.encode("utf-8")).hexdigest(),
                "previous_sha256": hashlib.sha256(
                    original.encode("utf-8")
                ).hexdigest(),
            },
        )
        self.assert_no_temporary_files(self.root)

    def test_digest_mismatch_preserves_original_and_cleans_temporary(self):
        target = self.root / "existing.txt"
        original = "original\n"
        target.write_text(original, encoding="utf-8")

        self.assert_error(
            self.execute(
                path="existing.txt",
                content="replacement\n",
                expected_sha256=hashlib.sha256(b"different\n").hexdigest(),
            ),
            "WRITE_HASH_MISMATCH",
        )

        self.assertEqual(target.read_text(encoding="utf-8"), original)
        self.assert_no_temporary_files(self.root)

    def test_target_changed_after_expected_digest_was_computed(self):
        target = self.root / "existing.txt"
        original = "original\n"
        target.write_text(original, encoding="utf-8")
        expected = hashlib.sha256(original.encode("utf-8")).hexdigest()
        target.write_text("changed elsewhere\n", encoding="utf-8")

        self.assert_error(
            self.execute(
                path="existing.txt",
                content="replacement\n",
                expected_sha256=expected,
            ),
            "WRITE_HASH_MISMATCH",
        )

        self.assertEqual(
            target.read_text(encoding="utf-8"), "changed elsewhere\n"
        )
        self.assert_no_temporary_files(self.root)

    def test_sensitive_and_escape_overwrite_paths_are_denied(self):
        secret = self.root / ".env"
        secret.write_text("TOKEN=original\n", encoding="utf-8")
        digest = hashlib.sha256(secret.read_bytes()).hexdigest()
        self.assert_error(
            self.execute(
                path=".env",
                content="TOKEN=replacement\n",
                expected_sha256=digest,
            ),
            "PATH_SENSITIVE",
        )
        self.assertEqual(secret.read_text(encoding="utf-8"), "TOKEN=original\n")

        with tempfile.TemporaryDirectory(prefix="rupi-write-file-outside-") as directory:
            outside = Path(directory)
            target = outside / "outside.txt"
            target.write_text("outside\n", encoding="utf-8")
            self.symlink(target, self.root / "outside-link")
            self.assert_error(
                self.execute(
                    path="outside-link",
                    content="replacement\n",
                    expected_sha256=hashlib.sha256(b"outside\n").hexdigest(),
                ),
                "PATH_SYMLINK_ESCAPE",
            )
            self.assertEqual(target.read_text(encoding="utf-8"), "outside\n")

    def test_failed_atomic_replace_keeps_original_and_cleans_temporary(self):
        target = self.root / "existing.txt"
        original = "original\n"
        target.write_text(original, encoding="utf-8")

        with mock.patch.object(
            write_file.os,
            "replace",
            side_effect=OSError(5, "forced replace failure"),
        ):
            with self.assertRaises(write_file.WriteError) as raised:
                write_file._execute(
                    self.request(
                        path="existing.txt",
                        content="replacement\n",
                        expected_sha256=hashlib.sha256(
                            original.encode("utf-8")
                        ).hexdigest(),
                    )
                )

        self.assertEqual(raised.exception.code, "WRITE_ATOMIC_REPLACE_FAILED")
        self.assertEqual(target.read_text(encoding="utf-8"), original)
        self.assert_no_temporary_files(self.root)

    def test_rejects_sensitive_traversal_and_symlink_escape_paths(self):
        with tempfile.TemporaryDirectory(prefix="rupi-write-file-outside-") as directory:
            outside = Path(directory)
            self.symlink(outside, self.root / "outside-link")
            for path, code in (
                (".env", "PATH_SENSITIVE"),
                (".ssh/id_rsa", "PATH_SENSITIVE"),
                ("../outside.txt", "PATH_TRAVERSAL"),
                ("/tmp/outside.txt", "PATH_ABSOLUTE"),
                ("outside-link/new.txt", "PATH_SYMLINK_ESCAPE"),
                ("broken-link/new.txt", "PATH_SYMLINK_ESCAPE"),
            ):
                with self.subTest(path=path):
                    if path.startswith("broken-link"):
                        self.symlink(outside / "missing", self.root / "broken-link")
                    self.assert_error(
                        self.execute(path=path, content="content\n"), code
                    )
            self.assertFalse((outside / "new.txt").exists())

    def test_rejects_invalid_params_and_content_bounds(self):
        too_large = "x" * (MAX_CONTENT_BYTES + 1)
        too_long_path = "a" * (MAX_PATH_BYTES + 1)
        cases = [
            ({"path": "file.txt", "content": too_large}, "WRITE_CONTENT_TOO_LARGE"),
            ({"path": "file.txt", "content": "nul\x00content"}, "WRITE_CONTENT_NUL"),
            ({"path": "file.txt", "content": "ok", "expected_sha256": "A" * 64}, "INVALID_PARAMS"),
            ({"path": too_long_path, "content": "ok"}, "WRITE_PATH_TOO_LONG"),
            ({"path": "nul\x00path", "content": "ok"}, "PATH_NUL"),
            ({"path": "control\npath", "content": "ok"}, "WRITE_PATH_CONTROL"),
            ({"path": "file.txt", "content": 42}, "INVALID_PARAMS"),
            ({"path": "file.txt", "content": "ok", "expected_sha256": 1}, "INVALID_PARAMS"),
            ({"path": "file.txt", "content": "ok", "extra": True}, "INVALID_PARAMS"),
            ({"path": "file.txt"}, "INVALID_PARAMS"),
        ]
        for params, code in cases:
            with self.subTest(code=code):
                self.assert_error(self.execute(**params), code)
        self.assertFalse((self.root / "file.txt").exists())

    def test_failed_publish_removes_temporary_file_and_created_parents(self):
        with mock.patch.object(
            write_file.os,
            "link",
            side_effect=OSError(5, "forced publish failure"),
        ):
            with self.assertRaises(write_file.WriteError) as raised:
                write_file._execute(self.request(path="new/nested/file.txt", content="data"))
        self.assertEqual(raised.exception.code, "WRITE_ATOMIC_PUBLISH_FAILED")
        self.assertFalse((self.root / "new").exists())
        self.assertFalse((self.root / "new/nested/file.txt").exists())

    def test_rejects_malformed_protocol_requests(self):
        cases = [
            (
                {
                    "protocol_version": 2,
                    "action": "write_file",
                    "project_root": str(self.root),
                    "params": {"path": "x", "content": "y"},
                },
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "read_file",
                    "project_root": str(self.root),
                    "params": {"path": "x", "content": "y"},
                },
                "INVALID_ACTION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "write_file",
                    "params": {"path": "x", "content": "y"},
                },
                "INVALID_REQUEST",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "write_file",
                    "project_root": str(self.root),
                    "params": [],
                },
                "INVALID_PARAMS",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "write_file",
                    "project_root": str(self.root),
                    "params": {"path": "x", "content": "y"},
                    "extra": True,
                },
                "INVALID_REQUEST",
            ),
        ]
        for request, code in cases:
            with self.subTest(code=code):
                response = self.run_raw(json.dumps(request).encode("utf-8"))
                self.assert_error(response, code)

        self.assert_error(self.run_raw(b"{not json"), "INVALID_REQUEST")

    def test_in_root_symlinked_parent_is_allowed(self):
        (self.root / "real").mkdir()
        self.symlink(self.root / "real", self.root / "alias")
        data = self.assert_success(
            self.execute(path="alias/new.txt", content="content\n")
        )
        self.assertEqual(data["path"], "alias/new.txt")
        self.assertEqual((self.root / "real/new.txt").read_text(encoding="utf-8"), "content\n")


if __name__ == "__main__":
    unittest.main()
