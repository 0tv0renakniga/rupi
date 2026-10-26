import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

from read_file import MAX_CONTENT_BYTES  # noqa: E402


class ReadFileScriptTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-read-file-")
        self.root = Path(self._temp.name)

    def tearDown(self):
        self._temp.cleanup()

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, str):
            path.write_text(content, encoding="utf-8")
        else:
            path.write_bytes(content)
        return path

    def request(self, **params):
        return {
            "protocol_version": 1,
            "action": "read_file",
            "project_root": str(self.root),
            "params": params,
        }

    def run_raw(self, raw):
        completed = subprocess.run(
            [sys.executable, str(SCRIPTS / "read_file.py")],
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
        self.assertLessEqual(
            len(response["error"]["message"].encode("utf-8")), 1024
        )

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

    def test_reads_complete_utf8_file_without_mutation(self):
        path = self.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n")
        before = path.read_bytes()
        before_stat = path.stat()

        data = self.assert_success(self.execute(path="src/lib.rs"))

        self.assertEqual(
            data,
            {
                "path": "src/lib.rs",
                "content": "pub fn value() -> u8 { 1 }\n",
                "line_start": 1,
                "line_end": 1,
                "next_line": None,
                "truncated": False,
                "partial_line": False,
            },
        )
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(path.stat().st_mtime_ns, before_stat.st_mtime_ns)

    def test_handles_files_without_a_trailing_newline(self):
        self.write("notes.txt", "one\ntwo\nthree")
        data = self.assert_success(self.execute(path="notes.txt"))
        self.assertEqual(data["content"], "one\ntwo\nthree")
        self.assertEqual(data["line_start"], 1)
        self.assertEqual(data["line_end"], 3)

    def test_line_ranges_are_one_based_and_inclusive(self):
        self.write("lines.txt", "one\ntwo\nthree\n")

        middle = self.assert_success(
            self.execute(path="lines.txt", line_start=2, line_end=2)
        )
        self.assertEqual(middle["content"], "two\n")
        self.assertEqual(middle["line_start"], 2)
        self.assertEqual(middle["line_end"], 2)

        from_two = self.assert_success(self.execute(path="lines.txt", line_start=2))
        self.assertEqual(from_two["content"], "two\nthree\n")
        self.assertEqual(from_two["line_end"], 3)

        through_two = self.assert_success(self.execute(path="lines.txt", line_end=2))
        self.assertEqual(through_two["content"], "one\ntwo\n")
        self.assertEqual(through_two["line_end"], 2)

    def test_line_ranges_beyond_eof_are_empty_successes(self):
        self.write("lines.txt", "one\ntwo\n")
        data = self.assert_success(self.execute(path="lines.txt", line_start=99))
        self.assertEqual(data["content"], "")
        self.assertIsNone(data["line_start"])
        self.assertIsNone(data["line_end"])
        self.assertIsNone(data["next_line"])
        self.assertFalse(data["truncated"])

    def test_rejects_invalid_line_ranges_and_params(self):
        self.write("lines.txt", "one\ntwo\nthree\n")
        cases = [
            {"path": "lines.txt", "line_start": 0},
            {"path": "lines.txt", "line_start": -1},
            {"path": "lines.txt", "line_start": True},
            {"path": "lines.txt", "line_start": "2"},
            {"path": "lines.txt", "line_start": 2.5},
            {"path": "lines.txt", "line_start": 0x1_0000_0000},
            {"path": "lines.txt", "line_start": 3, "line_end": 2},
            {"path": "lines.txt", "line_end": 0},
            {"path": "lines.txt", "line_start": None},
            {"path": 42},
            {"offset": 1},
            {},
        ]
        for params in cases:
            with self.subTest(params=params):
                self.assert_error(self.execute(**params), "INVALID_PARAMS")

    def test_truncates_large_selections_at_line_boundaries(self):
        lines = [f"{number:05d} " + ("x" * 80) + "\n" for number in range(2000)]
        full = "".join(lines)
        self.write("large.txt", full)

        data = self.assert_success(self.execute(path="large.txt"))
        self.assertTrue(data["truncated"])
        self.assertEqual(data["line_start"], 1)
        self.assertIsNotNone(data["line_end"])
        self.assertEqual(data["next_line"], data["line_end"] + 1)
        self.assertFalse(data["partial_line"])
        self.assertLessEqual(len(data["content"].encode("utf-8")), MAX_CONTENT_BYTES)
        self.assertTrue(full.startswith(data["content"]))
        self.assertTrue(data["content"].endswith("\n"))
        self.assertEqual(data["content"].count("\n"), data["line_end"])

    def test_truncates_a_single_line_at_a_utf8_boundary(self):
        content = "a" + ("é" * MAX_CONTENT_BYTES)
        self.write("one-line.txt", content)
        data = self.assert_success(self.execute(path="one-line.txt"))
        self.assertTrue(data["truncated"])
        self.assertTrue(data["partial_line"])
        self.assertEqual(data["line_start"], 1)
        self.assertEqual(data["line_end"], 1)
        self.assertEqual(data["next_line"], 2)
        self.assertLessEqual(len(data["content"].encode("utf-8")), MAX_CONTENT_BYTES)
        self.assertTrue(content.startswith(data["content"]))
        self.assertGreater(len(data["content"]), 0)

    def test_rejects_non_utf8_selected_content(self):
        self.write("binary.dat", b"one\n\xff\xfe\ntwo\n")
        self.assert_error(self.execute(path="binary.dat"), "READ_NOT_UTF8")

    def test_denies_sensitive_paths(self):
        self.write(".env", "SECRET=1\n")
        self.write("nested/.env.production", "SECRET=1\n")
        self.write("credentials.json", "{}\n")
        for path in (".env", "nested/.env.production", "credentials.json", ".ssh/id_rsa"):
            with self.subTest(path=path):
                self.assert_error(self.execute(path=path), "PATH_SENSITIVE")

    def test_rejects_lexical_path_escapes(self):
        self.write("inside.txt", "inside\n")
        cases = [
            ("", "PATH_EMPTY"),
            ("   ", "PATH_EMPTY"),
            (".", "PATH_EMPTY"),
            ("/etc/passwd", "PATH_ABSOLUTE"),
            ("../outside.txt", "PATH_TRAVERSAL"),
            ("nested/../../outside.txt", "PATH_TRAVERSAL"),
            ("nul\x00path", "PATH_NUL"),
        ]
        for path, code in cases:
            with self.subTest(path=path):
                self.assert_error(self.execute(path=path), code)

    def test_rejects_symlink_escapes_and_allows_in_root_symlinks(self):
        source = self.write("source.txt", "inside\n")
        self.symlink(source, self.root / "inside-link")

        with tempfile.TemporaryDirectory(prefix="rupi-read-file-outside-") as directory:
            outside = Path(directory) / "outside.txt"
            outside.write_text("outside\n", encoding="utf-8")
            self.symlink(outside, self.root / "escape-link")
            self.assert_error(self.execute(path="escape-link"), "PATH_SYMLINK_ESCAPE")

        data = self.assert_success(self.execute(path="inside-link"))
        self.assertEqual(data["content"], "inside\n")
        self.assertEqual(data["path"], "inside-link")

    def test_rejects_directories(self):
        (self.root / "src").mkdir()
        self.assert_error(self.execute(path="src"), "READ_NOT_FILE")

    def test_rejects_malformed_protocol_requests(self):
        cases = [
            (
                {
                    "protocol_version": 2,
                    "action": "read_file",
                    "project_root": str(self.root),
                    "params": {"path": "x"},
                },
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "write_file",
                    "project_root": str(self.root),
                    "params": {"path": "x"},
                },
                "INVALID_ACTION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "read_file",
                    "params": {"path": "x"},
                },
                "INVALID_REQUEST",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "read_file",
                    "project_root": str(self.root),
                    "params": [],
                },
                "INVALID_PARAMS",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "read_file",
                    "project_root": str(self.root),
                    "params": {"path": "x"},
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
        self.assert_error(self.run_raw(b"[]"), "INVALID_REQUEST")

    def test_rejects_oversized_requests(self):
        request = self.request(path="x")
        request["params"]["blob"] = "x" * (64 * 1024)
        self.assert_error(
            self.run_raw(json.dumps(request).encode("utf-8")), "REQUEST_TOO_LARGE"
        )


if __name__ == "__main__":
    unittest.main()
