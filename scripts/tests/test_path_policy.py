import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from path_policy import MAX_PATH_ERROR_BYTES, PathPolicy, PathPolicyError  # noqa: E402


class PathPolicyTests(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="rupi-script-path-policy-")
        self.root = Path(self._temp.name)
        self.policy = PathPolicy(self.root)

    def tearDown(self):
        self._temp.cleanup()

    def assert_code(self, raw, code, authorize=None):
        authorize = authorize or self.policy.authorize_path
        with self.assertRaises(PathPolicyError) as raised:
            authorize(raw)
        self.assertEqual(raised.exception.code, code)
        return raised.exception

    def symlink(self, target, link):
        try:
            os.symlink(target, link)
        except (OSError, NotImplementedError):
            self.skipTest("symlink creation is unavailable")

    def test_canonicalizes_root_and_accepts_nested_paths(self):
        self.assertEqual(self.policy.root, self.root.resolve())
        self.assertEqual(self.policy.authorize_path("src/main.rs"), Path("src/main.rs"))
        self.assertEqual(
            self.policy.authorize_path("src/../README.md"), Path("README.md")
        )

    def test_rejects_empty_nul_absolute_and_traversal(self):
        self.assert_code("", "PATH_EMPTY")
        self.assert_code("   ", "PATH_EMPTY")
        self.assert_code("src/\x00main.rs", "PATH_NUL")
        self.assert_code("/etc/passwd", "PATH_ABSOLUTE")
        for path in ("..", "../Cargo.toml", "src/../../outside.txt"):
            self.assert_code(path, "PATH_TRAVERSAL")

    def test_denies_sensitive_harness_private_and_credential_paths(self):
        for path in (
            ".env",
            ".env.local",
            "nested/.env.production",
            ".ssh/id_rsa",
            "credentials.json",
            ".aws/credentials",
            ".gcp/credentials.json",
            "config/credentials.yaml",
            "secrets.yaml",
            "secrets/prod.txt",
            "nested/.env/config",
            "certs/server.pem",
            "keys/signing.key",
            ".harness-audit/audit.jsonl",
            ".ai-wiki/state-of-the-world.md",
        ):
            self.assert_code(path, "PATH_SENSITIVE")

    def test_component_matching_does_not_use_substrings(self):
        for path in (
            "src/environment.rs",
            "docs/secret-management.md",
            "tests/keyboard.rs",
            "src/credentials_policy.rs",
        ):
            self.assertIsInstance(self.policy.authorize_path(path), Path)

    def test_existing_and_new_file_apis_share_semantics(self):
        for authorize in (
            self.policy.authorize_existing_path,
            self.policy.authorize_new_file_path,
        ):
            self.assertEqual(authorize("src/new_module.rs"), Path("src/new_module.rs"))
            self.assert_code("../outside.txt", "PATH_TRAVERSAL", authorize)
            self.assert_code(".ai-wiki/plans/task.md", "PATH_SENSITIVE", authorize)

    def test_rejects_existing_and_parent_symlink_escapes(self):
        with tempfile.TemporaryDirectory(prefix="rupi-script-path-outside-") as directory:
            outside = Path(directory)
            (outside / "secret.txt").write_text("outside", encoding="utf-8")
            self.symlink(outside / "secret.txt", self.root / "linked-file")
            self.symlink(outside, self.root / "linked-parent")
            self.symlink(outside / "missing.txt", self.root / "broken-link")
            self.symlink("loop", self.root / "loop")

            for path in ("linked-file", "linked-parent/secret.txt"):
                self.assert_code(
                    path, "PATH_SYMLINK_ESCAPE", self.policy.authorize_existing_path
                )
            for path in ("linked-parent/new.txt", "broken-link", "loop"):
                self.assert_code(
                    path, "PATH_SYMLINK_ESCAPE", self.policy.authorize_new_file_path
                )
            self.assert_code("linked-parent/secret.txt", "PATH_SYMLINK_ESCAPE")

    def test_allows_in_root_symlinks_and_missing_nested_parents(self):
        (self.root / "real").mkdir()
        (self.root / "real" / "inside.txt").write_text("inside", encoding="utf-8")
        self.symlink(self.root / "real" / "inside.txt", self.root / "inside-link")
        self.symlink(self.root / "real", self.root / "real-link")

        self.assertEqual(
            self.policy.authorize_existing_path("inside-link"), Path("inside-link")
        )
        self.assertEqual(
            self.policy.authorize_existing_path("real-link/inside.txt"),
            Path("real-link/inside.txt"),
        )
        self.assertEqual(
            self.policy.authorize_new_file_path("real-link/new.txt"),
            Path("real-link/new.txt"),
        )
        self.assertEqual(
            self.policy.authorize_new_file_path("missing/nested/file.txt"),
            Path("missing/nested/file.txt"),
        )

    def test_symlinks_cannot_bypass_sensitive_paths(self):
        (self.root / ".env").write_text("SECRET", encoding="utf-8")
        (self.root / ".ssh").mkdir()
        (self.root / ".ssh" / "key").write_text("SECRET", encoding="utf-8")
        self.symlink(self.root / ".env", self.root / "env-link")
        self.symlink(self.root / ".ssh", self.root / "ssh-link")

        self.assert_code("env-link", "PATH_SENSITIVE", self.policy.authorize_existing_path)
        self.assert_code(
            "ssh-link/new-key", "PATH_SENSITIVE", self.policy.authorize_new_file_path
        )

    def test_root_errors_have_stable_codes(self):
        missing = self.root / "missing"
        with self.assertRaises(PathPolicyError) as raised:
            PathPolicy(missing)
        self.assertEqual(raised.exception.code, "PATH_ROOT_INACCESSIBLE")

        file = self.root / "not-a-directory"
        file.write_text("not a directory", encoding="utf-8")
        with self.assertRaises(PathPolicyError) as raised:
            PathPolicy(file)
        self.assertEqual(raised.exception.code, "PATH_ROOT_NOT_DIRECTORY")

    def test_symlink_loop_root_has_stable_code(self):
        loop = self.root / "loop"
        try:
            os.symlink("loop", loop)
        except (OSError, NotImplementedError):
            self.skipTest("symlink creation is unavailable")

        with self.assertRaises(PathPolicyError) as raised:
            PathPolicy(loop)
        self.assertEqual(raised.exception.code, "PATH_ROOT_INACCESSIBLE")

    def test_errors_are_bounded_and_control_safe(self):
        error = self.assert_code("/" + "x" * 2000 + "\n" + "y" * 2000, "PATH_ABSOLUTE")
        message = str(error)
        self.assertLessEqual(len(message.encode("utf-8")), MAX_PATH_ERROR_BYTES + 128)
        self.assertNotIn("\n", message)


if __name__ == "__main__":
    unittest.main()
