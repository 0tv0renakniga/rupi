import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

from git_status import MAX_STATUS_BYTES  # noqa: E402


GIT = shutil.which("git")


def git_environment():
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_AUTHOR_NAME": "Rupi Test",
            "GIT_AUTHOR_EMAIL": "rupi-test@example.invalid",
            "GIT_COMMITTER_NAME": "Rupi Test",
            "GIT_COMMITTER_EMAIL": "rupi-test@example.invalid",
        }
    )
    return environment


@unittest.skipIf(GIT is None, "git executable is unavailable")
class GitStatusScriptTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-git-status-")
        self.base = Path(self._temp.name)
        self.root = self.base / "project"
        self.root.mkdir()

    def tearDown(self):
        self._temp.cleanup()

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def git(self, *args):
        return subprocess.run(
            [GIT, *args],
            cwd=self.root,
            env=git_environment(),
            capture_output=True,
            text=True,
            check=True,
        )

    def init_repository(self):
        self.git("init", "--quiet")
        self.write("tracked.txt", "initial\n")
        self.git("add", "tracked.txt")
        self.git("commit", "--quiet", "-m", "initial")

    def request(self, **params):
        request = {
            "protocol_version": 1,
            "action": "git_status",
            "project_root": str(self.root),
        }
        if params:
            request["params"] = params
        return request

    def run_raw(self, raw, env=None):
        completed = subprocess.run(
            [sys.executable, str(SCRIPTS / "git_status.py")],
            input=raw,
            cwd=self.root,
            env=env,
            capture_output=True,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        self.assertEqual(completed.stderr, b"")
        return json.loads(completed.stdout)

    def execute(self, **params):
        return self.run_raw(json.dumps(self.request(**params)).encode("utf-8"))

    def assert_success(self, response):
        self.assertEqual(response["protocol_version"], 1)
        self.assertTrue(response["ok"])
        self.assertNotIn("error", response)
        return response["data"]

    def assert_error(self, response, code):
        self.assertEqual(response["protocol_version"], 1)
        self.assertFalse(response["ok"])
        self.assertNotIn("data", response)
        self.assertEqual(response["error"]["code"], code)
        self.assertTrue(response["error"]["message"])
        self.assertLessEqual(
            len(response["error"]["message"].encode("utf-8")), 1024
        )

    def install_fake_git(self, body):
        directory = self.base / "fake-bin"
        directory.mkdir()
        script = directory / "git"
        script.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
        script.chmod(0o755)
        return directory

    def test_clean_repository(self):
        self.init_repository()
        data = self.assert_success(self.execute())
        self.assertEqual(
            data,
            {"clean": True, "status": "", "truncated": False},
        )

    def test_modified_staged_and_untracked_files_are_reported(self):
        self.init_repository()
        self.write("tracked.txt", "modified\n")
        self.write("staged.txt", "staged\n")
        self.git("add", "staged.txt")
        self.write("untracked.txt", "untracked\n")

        data = self.assert_success(self.execute())
        self.assertFalse(data["clean"])
        self.assertFalse(data["truncated"])
        entries = set(data["status"].splitlines())
        self.assertIn(" M tracked.txt", entries)
        self.assertIn("A  staged.txt", entries)
        self.assertIn("?? untracked.txt", entries)

    def test_non_repository_root_is_a_clear_error(self):
        self.assert_error(self.execute(), "NOT_A_GIT_REPOSITORY")

    def test_status_does_not_mutate_repository_state(self):
        self.init_repository()
        self.write("tracked.txt", "modified\n")
        self.write("untracked.txt", "untracked\n")
        index = self.root / ".git" / "index"
        head = self.root / ".git" / "HEAD"
        index_before = index.read_bytes()
        head_before = head.read_bytes()
        tracked_before = (self.root / "tracked.txt").read_bytes()

        data = self.assert_success(self.execute())

        self.assertFalse(data["clean"])
        self.assertEqual(index.read_bytes(), index_before)
        self.assertEqual(head.read_bytes(), head_before)
        self.assertEqual((self.root / "tracked.txt").read_bytes(), tracked_before)
        self.assertFalse((self.root / ".git" / "index.lock").exists())

    def test_git_runs_with_fixed_argv_and_minimal_environment(self):
        self.init_repository()
        record_path = self.base / "git-record.json"
        wrapper = self.install_fake_git(
            f"""
import json
import os
import sys

with open({str(record_path)!r}, "w", encoding="utf-8") as handle:
    json.dump(
        {{
            "argv": sys.argv[1:],
            "cwd": os.getcwd(),
            "env": dict(os.environ),
        }},
        handle,
    )
os.execv({GIT!r}, [{GIT!r}, *sys.argv[1:]])
"""
        )

        environment = os.environ.copy()
        environment["PATH"] = f"{wrapper}{os.pathsep}{environment['PATH']}"
        environment["SENTINEL_SECRET"] = "must-not-reach-git"
        environment["GIT_PAGER"] = "less"
        environment["PAGER"] = "less"
        environment["GIT_OPTIONAL_LOCKS"] = "1"

        data = self.assert_success(
            self.run_raw(json.dumps(self.request()).encode("utf-8"), env=environment)
        )
        self.assertTrue(data["clean"])

        record = json.loads(record_path.read_text(encoding="utf-8"))
        argv = record["argv"]
        self.assertEqual(argv[0], "--no-pager")
        self.assertIn("status", argv)
        self.assertIn("--porcelain=v1", argv)
        self.assertIn("--untracked-files=normal", argv)
        self.assertEqual(argv[-2:], ["--", "."])
        self.assertNotIn("sh", argv)
        self.assertNotIn("bash", argv)
        self.assertEqual(Path(record["cwd"]).resolve(), self.root.resolve())

        environment_record = record["env"]
        self.assertEqual(environment_record["GIT_OPTIONAL_LOCKS"], "0")
        self.assertEqual(environment_record["GIT_PAGER"], "cat")
        self.assertEqual(environment_record["PAGER"], "cat")
        self.assertEqual(environment_record["GIT_CONFIG_NOSYSTEM"], "1")
        self.assertEqual(environment_record["GIT_CONFIG_GLOBAL"], os.devnull)
        self.assertNotIn("SENTINEL_SECRET", environment_record)

    def test_output_is_capped_and_marked_truncated(self):
        wrapper = self.install_fake_git(
            """
import sys

line = "?? " + ("x" * 200) + "\\n"
sys.stdout.write(line * 2000)
"""
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{wrapper}{os.pathsep}{environment['PATH']}"

        data = self.assert_success(
            self.run_raw(json.dumps(self.request()).encode("utf-8"), env=environment)
        )
        self.assertFalse(data["clean"])
        self.assertTrue(data["truncated"])
        self.assertLessEqual(len(data["status"].encode("utf-8")), MAX_STATUS_BYTES)
        self.assertTrue(data["status"].startswith("?? "))

    def test_status_output_is_display_safe(self):
        wrapper = self.install_fake_git(
            """
import sys

sys.stdout.write(" M ok\\x1b[31mred\\x1b[0m\\n")
"""
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{wrapper}{os.pathsep}{environment['PATH']}"

        data = self.assert_success(
            self.run_raw(json.dumps(self.request()).encode("utf-8"), env=environment)
        )
        self.assertNotIn("\x1b", data["status"])
        self.assertIn(" M ok", data["status"])

    def test_rejects_malformed_protocol_requests(self):
        cases = [
            (
                {
                    "protocol_version": 2,
                    "action": "git_status",
                    "project_root": str(self.root),
                },
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_diff",
                    "project_root": str(self.root),
                },
                "INVALID_ACTION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_status",
                },
                "INVALID_REQUEST",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_status",
                    "project_root": str(self.root),
                    "params": [],
                },
                "INVALID_PARAMS",
            ),
            (
                self.request(staged=False),
                "INVALID_PARAMS",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_status",
                    "project_root": str(self.root),
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

    def test_missing_or_non_directory_root_is_rejected(self):
        self.assert_error(
            self.run_raw(
                json.dumps(
                    {
                        "protocol_version": 1,
                        "action": "git_status",
                        "project_root": str(self.base / "missing"),
                    }
                ).encode("utf-8")
            ),
            "PATH_ROOT_INACCESSIBLE",
        )
        self.assert_error(
            self.run_raw(
                json.dumps(
                    {
                        "protocol_version": 1,
                        "action": "git_status",
                        "project_root": str(self.write("not-a-directory.txt", "x")),
                    }
                ).encode("utf-8")
            ),
            "PATH_ROOT_NOT_DIRECTORY",
        )


if __name__ == "__main__":
    unittest.main()
