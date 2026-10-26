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

from git_diff import MAX_DIFF_BYTES  # noqa: E402


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
class GitDiffScriptTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-git-diff-")
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

    def request(self, staged=False, include_staged=True, **params):
        values = dict(params)
        if include_staged:
            values["staged"] = staged
        request = {
            "protocol_version": 1,
            "action": "git_diff",
            "project_root": str(self.root),
        }
        if values:
            request["params"] = values
        return request

    def run_raw(self, raw, env=None):
        completed = subprocess.run(
            [sys.executable, str(SCRIPTS / "git_diff.py")],
            input=raw,
            cwd=self.root,
            env=env,
            capture_output=True,
            timeout=30,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        self.assertEqual(completed.stderr, b"")
        return json.loads(completed.stdout)

    def execute(self, staged=False):
        return self.run_raw(
            json.dumps(self.request(staged=staged)).encode("utf-8")
        )

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
        directory.mkdir(exist_ok=True)
        script = directory / "git"
        script.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
        script.chmod(0o755)
        return directory

    def test_clean_repository_has_an_empty_diff(self):
        self.init_repository()
        for staged in (False, True):
            with self.subTest(staged=staged):
                data = self.assert_success(self.execute(staged=staged))
                self.assertEqual(
                    data,
                    {"staged": staged, "diff": "", "truncated": False},
                )

    def test_unstaged_and_staged_diffs_are_separate(self):
        self.init_repository()
        self.write("tracked.txt", "unstaged change\n")
        self.write("staged.txt", "staged change\n")
        self.git("add", "staged.txt")

        unstaged = self.assert_success(self.execute(staged=False))
        self.assertFalse(unstaged["staged"])
        self.assertFalse(unstaged["truncated"])
        self.assertIn("diff --git a/tracked.txt b/tracked.txt", unstaged["diff"])
        self.assertIn("+unstaged change", unstaged["diff"])
        self.assertNotIn("staged.txt", unstaged["diff"])

        staged = self.assert_success(self.execute(staged=True))
        self.assertTrue(staged["staged"])
        self.assertFalse(staged["truncated"])
        self.assertIn("diff --git a/staged.txt b/staged.txt", staged["diff"])
        self.assertIn("+staged change", staged["diff"])
        self.assertNotIn("tracked.txt", staged["diff"])

    def test_non_repository_root_is_a_clear_error(self):
        self.assert_error(self.execute(), "NOT_A_GIT_REPOSITORY")

    def test_rejects_invalid_argument_shapes(self):
        cases = [
            (
                {
                    "protocol_version": 2,
                    "action": "git_diff",
                    "project_root": str(self.root),
                    "params": {"staged": False},
                },
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_status",
                    "project_root": str(self.root),
                    "params": {"staged": False},
                },
                "INVALID_ACTION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_diff",
                },
                "INVALID_REQUEST",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_diff",
                    "project_root": str(self.root),
                    "params": [],
                },
                "INVALID_PARAMS",
            ),
            (
                self.request(include_staged=False),
                "INVALID_PARAMS",
            ),
            (
                self.request(staged=0),
                "INVALID_PARAMS",
            ),
            (
                self.request(staged=1),
                "INVALID_PARAMS",
            ),
            (
                self.request(staged="false"),
                "INVALID_PARAMS",
            ),
            (
                self.request(staged=None),
                "INVALID_PARAMS",
            ),
            (
                self.request(staged=False, extra=True),
                "INVALID_PARAMS",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "git_diff",
                    "project_root": str(self.root),
                    "extra": True,
                    "params": {"staged": False},
                },
                "INVALID_REQUEST",
            ),
        ]
        for request, code in cases:
            with self.subTest(code=code, request=request):
                response = self.run_raw(json.dumps(request).encode("utf-8"))
                self.assert_error(response, code)

        self.assert_error(self.run_raw(b"{not json"), "INVALID_REQUEST")
        self.assert_error(self.run_raw(b"[]"), "INVALID_REQUEST")

    def test_large_diff_is_capped_with_head_and_tail(self):
        wrapper = self.install_fake_git(
            """
import sys

sys.stdout.write("HEAD-MARKER\\n")
sys.stdout.write(("x" * 200 + "\\n") * 2000)
sys.stdout.write("TAIL-MARKER\\n")
"""
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{wrapper}{os.pathsep}{environment['PATH']}"

        data = self.assert_success(
            self.run_raw(
                json.dumps(self.request(staged=False)).encode("utf-8"),
                env=environment,
            )
        )
        self.assertFalse(data["staged"])
        self.assertTrue(data["truncated"])
        self.assertLessEqual(len(data["diff"].encode("utf-8")), MAX_DIFF_BYTES)
        self.assertTrue(data["diff"].startswith("HEAD-MARKER\n"))
        self.assertTrue(data["diff"].endswith("TAIL-MARKER\n"))
        self.assertIn("[diff truncated]", data["diff"])

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
        environment["GIT_EXTERNAL_DIFF"] = "must-not-reach-git"
        environment["GIT_PAGER"] = "less"
        environment["PAGER"] = "less"
        environment["GIT_OPTIONAL_LOCKS"] = "1"

        for staged in (False, True):
            with self.subTest(staged=staged):
                self.assert_success(
                    self.run_raw(
                        json.dumps(self.request(staged=staged)).encode("utf-8"),
                        env=environment,
                    )
                )
                record = json.loads(record_path.read_text(encoding="utf-8"))
                argv = record["argv"]
                self.assertEqual(argv[0], "--no-pager")
                self.assertIn("diff", argv)
                self.assertEqual("--cached" in argv, staged)
                self.assertIn("--no-ext-diff", argv)
                self.assertIn("--no-textconv", argv)
                self.assertIn("--no-color", argv)
                self.assertEqual(argv[-2:], ["--", "."])
                self.assertNotIn("sh", argv)
                self.assertNotIn("bash", argv)
                self.assertEqual(Path(record["cwd"]).resolve(), self.root.resolve())

                environment_record = record["env"]
                self.assertEqual(environment_record["GIT_OPTIONAL_LOCKS"], "0")
                self.assertEqual(environment_record["GIT_PAGER"], "cat")
                self.assertEqual(environment_record["PAGER"], "cat")
                self.assertEqual(environment_record["GIT_CONFIG_NOSYSTEM"], "1")
                self.assertEqual(
                    environment_record["GIT_CONFIG_GLOBAL"], os.devnull
                )
                self.assertNotIn("SENTINEL_SECRET", environment_record)
                self.assertNotIn("GIT_EXTERNAL_DIFF", environment_record)

    def test_external_diff_and_textconv_are_not_executed(self):
        self.init_repository()
        self.write(".gitattributes", "*.txt diff=custom\n")
        self.git("add", ".gitattributes")
        self.git("commit", "--quiet", "-m", "attributes")

        textconv_marker = self.base / "textconv-ran"
        textconv = self.base / "textconv.py"
        textconv.write_text(
            f"""#!{sys.executable}
from pathlib import Path

Path({str(textconv_marker)!r}).write_text("ran", encoding="utf-8")
print("TEXTCONV")
""",
            encoding="utf-8",
        )
        textconv.chmod(0o755)
        self.git("config", "diff.custom.textconv", str(textconv))

        self.write("tracked.txt", "modified\n")
        data = self.assert_success(self.execute(staged=False))
        self.assertIn("+modified", data["diff"])
        self.assertFalse(textconv_marker.exists())

        external_marker = self.base / "external-diff-ran"
        external = self.base / "external-diff.py"
        external.write_text(
            f"""#!{sys.executable}
from pathlib import Path

Path({str(external_marker)!r}).write_text("ran", encoding="utf-8")
print("EXTERNAL-DIFF")
""",
            encoding="utf-8",
        )
        external.chmod(0o755)
        self.git("config", "diff.external", str(external))

        data = self.assert_success(self.execute(staged=False))
        self.assertIn("+modified", data["diff"])
        self.assertFalse(external_marker.exists())

    def test_diff_does_not_mutate_repository_state(self):
        self.init_repository()
        self.write("tracked.txt", "modified\n")
        self.write("staged.txt", "staged\n")
        self.git("add", "staged.txt")
        index = self.root / ".git" / "index"
        head = self.root / ".git" / "HEAD"
        index_before = index.read_bytes()
        head_before = head.read_bytes()
        tracked_before = (self.root / "tracked.txt").read_bytes()

        self.assert_success(self.execute(staged=False))
        self.assert_success(self.execute(staged=True))

        self.assertEqual(index.read_bytes(), index_before)
        self.assertEqual(head.read_bytes(), head_before)
        self.assertEqual((self.root / "tracked.txt").read_bytes(), tracked_before)
        self.assertFalse((self.root / ".git" / "index.lock").exists())

    def test_diff_output_is_display_safe(self):
        wrapper = self.install_fake_git(
            """
import sys

sys.stdout.write("diff --git a/ok b/ok\\n")
sys.stdout.write("+\\x1b[31mred\\x1b[0m\\x00\\n")
"""
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{wrapper}{os.pathsep}{environment['PATH']}"

        data = self.assert_success(
            self.run_raw(
                json.dumps(self.request(staged=False)).encode("utf-8"),
                env=environment,
            )
        )
        self.assertNotIn("\x1b", data["diff"])
        self.assertNotIn("\x00", data["diff"])
        self.assertIn("+ [31mred [0m", data["diff"])

    def test_missing_or_non_directory_root_is_rejected(self):
        self.assert_error(
            self.run_raw(
                json.dumps(
                    {
                        "protocol_version": 1,
                        "action": "git_diff",
                        "project_root": str(self.base / "missing"),
                        "params": {"staged": False},
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
                        "action": "git_diff",
                        "project_root": str(
                            self.write("not-a-directory.txt", "x")
                        ),
                        "params": {"staged": True},
                    }
                ).encode("utf-8")
            ),
            "PATH_ROOT_NOT_DIRECTORY",
        )


if __name__ == "__main__":
    unittest.main()
