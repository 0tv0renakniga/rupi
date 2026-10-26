import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

from run_verifier import MAX_OUTPUT_CAP, MAX_TIMEOUT_MS  # noqa: E402


class RunVerifierScriptTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-run-verifier-")
        self.root = Path(self._temp.name)
        self.fixture = self.root / "fixture.py"
        self.fixture.write_text(
            """
import json
import os
import sys
import time

mode = sys.argv[1]
if mode == "argv":
    print(json.dumps(sys.argv[2:]))
    print(os.getcwd())
elif mode == "exit":
    print("stdout from fixture")
    print("stderr from fixture", file=sys.stderr)
    raise SystemExit(int(sys.argv[2]))
elif mode == "sleep":
    (sys.argv[2] and open(sys.argv[2], "w").write(str(os.getpid())))
    time.sleep(5)
elif mode == "output":
    sys.stdout.write("O" * 5000)
    sys.stderr.write("E" * 5000)
else:
    raise SystemExit(9)
""".strip()
            + "\n",
            encoding="utf-8",
        )

    def tearDown(self):
        self._temp.cleanup()

    def request(self, **params):
        return {
            "protocol_version": 1,
            "action": "run_verifier",
            "project_root": str(self.root),
            "params": params,
        }

    def command(self, mode, *args, timeout_ms=5000, output_cap=4096, **extra):
        params = {
            "program": sys.executable,
            "args": [str(self.fixture), mode, *args],
            "cwd": ".",
            "timeout_ms": timeout_ms,
            "output_cap": output_cap,
        }
        params.update(extra)
        return params

    def run_raw(self, raw):
        completed = subprocess.run(
            [sys.executable, str(SCRIPTS / "run_verifier.py")],
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

    def test_preserves_argv_without_a_shell(self):
        args = [
            "two words",
            "quote'and\"double",
            "$HOME",
            "semi;colon",
            ">redirect",
            "*",
            "",
        ]
        data = self.assert_success(
            self.execute(**self.command("argv", *args))
        )

        lines = data["stdout"].splitlines()
        self.assertEqual(json.loads(lines[0]), args)
        self.assertEqual(lines[1], str(self.root.resolve()))
        self.assertEqual(data["stderr"], "")
        self.assertEqual(data["exit_code"], 0)
        self.assertFalse(data["timed_out"])
        self.assertGreaterEqual(data["duration_ms"], 0)

    def test_passing_and_failing_exit_statuses_are_preserved(self):
        passed = self.assert_success(self.execute(**self.command("exit", "0")))
        self.assertEqual(passed["exit_code"], 0)
        self.assertIn("stdout from fixture", passed["stdout"])
        self.assertIn("stderr from fixture", passed["stderr"])

        failed = self.assert_success(self.execute(**self.command("exit", "7")))
        self.assertEqual(failed["exit_code"], 7)
        self.assertFalse(failed["timed_out"])

    def test_timeout_terminates_the_command(self):
        pid_file = self.root / "child.pid"
        started = time.monotonic()
        data = self.assert_success(
            self.execute(
                **self.command(
                    "sleep",
                    str(pid_file),
                    timeout_ms=1000,
                    output_cap=256,
                )
            )
        )
        elapsed = time.monotonic() - started

        self.assertTrue(data["timed_out"])
        self.assertEqual(data["exit_code"], -2)
        self.assertLess(elapsed, 5, "timeout must terminate promptly")
        pid = int(pid_file.read_text(encoding="utf-8"))
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.02)
        else:
            self.fail("timed-out verifier command is still running")

    def test_output_cap_bounds_both_streams(self):
        data = self.assert_success(
            self.execute(**self.command("output", output_cap=100))
        )

        self.assertEqual(data["exit_code"], 0)
        for stream in ("stdout", "stderr"):
            self.assertIn("bytes omitted", data[stream])
            self.assertLessEqual(len(data[stream].encode("utf-8")), 180)

    def test_structured_blocker_when_program_is_missing(self):
        self.assert_error(
            self.execute(
                program="definitely-not-a-real-verifier-program",
                args=[],
                cwd=".",
                timeout_ms=1000,
                output_cap=256,
            ),
            "VERIFIER_START_FAILED",
        )

    def test_rejects_shells_and_invalid_bounds(self):
        for program in ["sh", "bash", "/bin/zsh", "sudo", "env", "powershell.exe"]:
            with self.subTest(program=program):
                self.assert_error(
                    self.execute(
                        program=program,
                        args=["-c", "echo unsafe"],
                        cwd=".",
                        timeout_ms=1000,
                        output_cap=256,
                    ),
                    "VERIFIER_PROGRAM_FORBIDDEN",
                )

        cases = [
            (
                self.command("exit", "0", timeout_ms=0),
                "INVALID_PARAMS",
            ),
            (
                self.command("exit", "0", timeout_ms=MAX_TIMEOUT_MS + 1),
                "INVALID_PARAMS",
            ),
            (
                self.command("exit", "0", output_cap=MAX_OUTPUT_CAP + 1),
                "INVALID_PARAMS",
            ),
            (
                {
                    "program": sys.executable,
                    "args": "x",
                    "cwd": ".",
                    "timeout_ms": 1,
                    "output_cap": 1,
                },
                "INVALID_PARAMS",
            ),
            (
                {
                    "program": sys.executable,
                    "args": [],
                    "cwd": ".",
                    "timeout_ms": True,
                    "output_cap": 1,
                },
                "INVALID_PARAMS",
            ),
        ]
        for params, code in cases:
            with self.subTest(code=code):
                self.assert_error(self.execute(**params), code)

    def test_cwd_cannot_escape_the_project_root(self):
        with tempfile.TemporaryDirectory(prefix="rupi-run-verifier-outside-") as directory:
            outside = Path(directory)
            self.assert_error(
                self.execute(**self.command("exit", "0", cwd=str(outside))),
                "VERIFIER_CWD_ESCAPE",
            )
            os.symlink(outside, self.root / "outside-link")
            self.assert_error(
                self.execute(**self.command("exit", "0", cwd="outside-link")),
                "VERIFIER_CWD_ESCAPE",
            )

        self.assert_error(
            self.execute(**self.command("exit", "0", cwd="missing")),
            "VERIFIER_CWD_INACCESSIBLE",
        )

    def test_rejects_malformed_protocol_requests(self):
        cases = [
            (
                {
                    "protocol_version": 2,
                    "action": "run_verifier",
                    "project_root": str(self.root),
                    "params": self.command("exit", "0"),
                },
                "UNSUPPORTED_PROTOCOL_VERSION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "write_file",
                    "project_root": str(self.root),
                    "params": self.command("exit", "0"),
                },
                "INVALID_ACTION",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "run_verifier",
                    "params": self.command("exit", "0"),
                },
                "INVALID_REQUEST",
            ),
            (
                {
                    "protocol_version": 1,
                    "action": "run_verifier",
                    "project_root": str(self.root),
                    "params": {**self.command("exit", "0"), "extra": True},
                },
                "INVALID_PARAMS",
            ),
        ]
        for request, code in cases:
            with self.subTest(code=code):
                self.assert_error(
                    self.run_raw(json.dumps(request).encode("utf-8")), code
                )


if __name__ == "__main__":
    unittest.main()
