use std::path::{Path, PathBuf};

use local_agent_harness::mcp::AgentTool;
use local_agent_harness::path_policy::{MAX_PATH_ERROR_BYTES, PathPolicy, PathPolicyError};
use local_agent_harness::policy::Policy;
use uuid::Uuid;

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("rupi-path-policy-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp root");
        Self { path }
    }

    fn policy(&self) -> PathPolicy {
        PathPolicy::new(&self.path).expect("build path policy")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn code(error: PathPolicyError) -> &'static str {
    error.code()
}

#[test]
fn canonicalizes_the_root_and_accepts_valid_nested_paths() {
    let root = TempRoot::new("valid");
    let policy = root.policy();

    assert_eq!(
        policy.root(),
        root.path.canonicalize().expect("canonical root")
    );
    assert_eq!(
        policy.authorize_path("src/main.rs").expect("nested path"),
        PathBuf::from("src/main.rs")
    );
    assert_eq!(
        policy
            .authorize_path("src/../README.md")
            .expect("normalized path"),
        PathBuf::from("README.md")
    );
    assert_eq!(
        policy
            .authorize_path("./tests/nested/file.rs")
            .expect("dotted path"),
        PathBuf::from("tests/nested/file.rs")
    );
}

#[test]
fn rejects_empty_nul_absolute_and_traversal_paths_with_stable_codes() {
    let root = TempRoot::new("invalid");
    let policy = root.policy();

    assert_eq!(
        code(policy.authorize_path("").expect_err("empty path")),
        "PATH_EMPTY"
    );
    assert_eq!(
        code(policy.authorize_path("   ").expect_err("blank path")),
        "PATH_EMPTY"
    );
    assert_eq!(
        code(
            policy
                .authorize_path("src/\0main.rs")
                .expect_err("NUL byte")
        ),
        "PATH_NUL"
    );

    let absolute = if cfg!(windows) {
        r"C:\Windows\System32\drivers\etc\hosts"
    } else {
        "/etc/passwd"
    };
    assert_eq!(
        code(policy.authorize_path(absolute).expect_err("absolute path")),
        "PATH_ABSOLUTE"
    );

    for path in ["..", "../Cargo.toml", "src/../../outside.txt"] {
        assert_eq!(
            code(policy.authorize_path(path).expect_err("traversal")),
            "PATH_TRAVERSAL",
            "{path}"
        );
    }
}

#[test]
fn denies_sensitive_harness_private_and_credential_paths() {
    let root = TempRoot::new("sensitive");
    let policy = root.policy();

    for path in [
        ".env",
        ".env.local",
        "nested/.env.production",
        ".ssh/id_rsa",
        "home/user/.ssh/authorized_keys",
        "credentials.json",
        "config/credentials",
        "config/credentials/token.txt",
        ".aws/credentials",
        ".gcp/credentials.json",
        "config/credentials.yaml",
        "secrets.yaml",
        "secrets/prod.txt",
        "nested/.env/config",
        "certs/server.pem",
        "keys/signing.key",
        "certs/bundle.p12",
        ".harness-audit/audit.jsonl",
        "nested/.harness-audit/session.jsonl",
        ".ai-wiki/state-of-the-world.md",
        "nested/.ai-wiki/log.md",
    ] {
        assert_eq!(
            code(policy.authorize_path(path).expect_err("sensitive path")),
            "PATH_SENSITIVE",
            "{path}"
        );
    }
}

#[test]
fn uses_component_matching_not_substring_matching() {
    let root = TempRoot::new("substring");
    let policy = root.policy();

    for path in [
        "src/environment.rs",
        "docs/secret-management.md",
        "tests/keyboard.rs",
        "src/credentials_policy.rs",
        "src/wiki/compile.rs",
    ] {
        assert!(policy.authorize_path(path).is_ok(), "{path}");
    }
}

#[test]
fn existing_and_new_file_apis_share_root_and_sensitive_semantics() {
    let root = TempRoot::new("intents");
    let policy = root.policy();

    type Authorize = fn(&PathPolicy, &str) -> Result<PathBuf, PathPolicyError>;
    for authorize in [
        PathPolicy::authorize_existing_path as Authorize,
        PathPolicy::authorize_new_file_path as Authorize,
    ] {
        assert_eq!(
            authorize(&policy, "src/new_module.rs").expect("valid path"),
            PathBuf::from("src/new_module.rs")
        );
        assert_eq!(
            code(authorize(&policy, "../outside.txt").expect_err("traversal")),
            "PATH_TRAVERSAL"
        );
        assert_eq!(
            code(authorize(&policy, ".ai-wiki/plans/task.md").expect_err("private")),
            "PATH_SENSITIVE"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_existing_and_parent_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let root = TempRoot::new("symlink-escape");
    let outside = TempRoot::new("symlink-outside");
    std::fs::write(outside.path.join("secret.txt"), "outside").expect("outside secret");
    symlink(
        outside.path.join("secret.txt"),
        root.path.join("linked-file"),
    )
    .expect("file symlink");
    symlink(&outside.path, root.path.join("linked-parent")).expect("parent symlink");
    symlink(
        outside.path.join("missing.txt"),
        root.path.join("broken-link"),
    )
    .expect("broken symlink");
    symlink("loop", root.path.join("loop")).expect("symlink loop");

    let policy = root.policy();
    for path in ["linked-file", "linked-parent/secret.txt"] {
        assert_eq!(
            code(
                policy
                    .authorize_existing_path(path)
                    .expect_err("existing symlink escape")
            ),
            "PATH_SYMLINK_ESCAPE",
            "{path}"
        );
    }
    for path in ["linked-parent/new.txt", "broken-link", "loop"] {
        assert_eq!(
            code(
                policy
                    .authorize_new_file_path(path)
                    .expect_err("new-file symlink escape")
            ),
            "PATH_SYMLINK_ESCAPE",
            "{path}"
        );
    }
    assert_eq!(
        code(
            policy
                .authorize_path("linked-parent/secret.txt")
                .expect_err("generic symlink escape")
        ),
        "PATH_SYMLINK_ESCAPE"
    );
}

#[cfg(unix)]
#[test]
fn allows_in_root_symlinks_and_missing_nested_parents() {
    use std::os::unix::fs::symlink;

    let root = TempRoot::new("symlink-inside");
    std::fs::create_dir_all(root.path.join("real")).expect("real directory");
    std::fs::write(root.path.join("real/inside.txt"), "inside").expect("inside file");
    symlink(
        root.path.join("real/inside.txt"),
        root.path.join("inside-link"),
    )
    .expect("inside file symlink");
    symlink(root.path.join("real"), root.path.join("real-link")).expect("inside directory symlink");

    let policy = root.policy();
    assert_eq!(
        policy
            .authorize_existing_path("inside-link")
            .expect("inside file symlink"),
        PathBuf::from("inside-link")
    );
    assert_eq!(
        policy
            .authorize_existing_path("real-link/inside.txt")
            .expect("inside parent symlink"),
        PathBuf::from("real-link/inside.txt")
    );
    assert_eq!(
        policy
            .authorize_new_file_path("real-link/new.txt")
            .expect("new file under inside symlink"),
        PathBuf::from("real-link/new.txt")
    );
    assert_eq!(
        policy
            .authorize_new_file_path("missing/nested/file.txt")
            .expect("missing nested parent"),
        PathBuf::from("missing/nested/file.txt")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_bypass_sensitive_paths() {
    use std::os::unix::fs::symlink;

    let root = TempRoot::new("symlink-sensitive");
    std::fs::write(root.path.join(".env"), "SECRET").expect("sensitive file");
    std::fs::create_dir_all(root.path.join(".ssh")).expect("sensitive directory");
    std::fs::write(root.path.join(".ssh/key"), "SECRET").expect("sensitive key");
    symlink(root.path.join(".env"), root.path.join("env-link")).expect("sensitive file symlink");
    symlink(root.path.join(".ssh"), root.path.join("ssh-link")).expect("sensitive dir symlink");

    let policy = root.policy();
    assert_eq!(
        code(
            policy
                .authorize_existing_path("env-link")
                .expect_err("sensitive file alias")
        ),
        "PATH_SENSITIVE"
    );
    assert_eq!(
        code(
            policy
                .authorize_new_file_path("ssh-link/new-key")
                .expect_err("sensitive directory alias")
        ),
        "PATH_SENSITIVE"
    );
}

#[test]
fn root_construction_fails_closed_with_stable_codes() {
    let base = TempRoot::new("root-errors");
    let missing = base.path.join("missing");
    assert_eq!(
        code(PathPolicy::new(&missing).expect_err("missing root")),
        "PATH_ROOT_INACCESSIBLE"
    );

    let file = base.path.join("not-a-directory");
    std::fs::write(&file, "not a directory").expect("write file");
    assert_eq!(
        code(PathPolicy::new(&file).expect_err("file root")),
        "PATH_ROOT_NOT_DIRECTORY"
    );

    #[cfg(unix)]
    {
        let loop_path = base.path.join("loop");
        std::os::unix::fs::symlink("loop", &loop_path).expect("symlink loop");
        assert_eq!(
            code(PathPolicy::new(&loop_path).expect_err("symlink loop root")),
            "PATH_ROOT_INACCESSIBLE"
        );
    }
}

#[test]
fn policy_errors_are_bounded_and_control_safe() {
    let root = TempRoot::new("bounded-error");
    let policy = root.policy();
    let raw = format!("/{}\n{}", "x".repeat(2_000), "y".repeat(2_000));
    let error = policy.authorize_path(&raw).expect_err("absolute path");
    let message = error.to_string();

    assert_eq!(error.code(), "PATH_ABSOLUTE");
    assert!(message.len() <= MAX_PATH_ERROR_BYTES + 128);
    assert!(!message.contains('\n'));
    assert!(!message.contains('\r'));
}

#[test]
fn canonical_root_cannot_be_supplied_as_a_relative_escape() {
    let root = TempRoot::new("root-path");
    let policy = root.policy();
    let absolute = root.path.join("src/lib.rs");
    let raw = absolute.to_string_lossy();

    assert_eq!(
        code(
            policy
                .authorize_path(raw.as_ref())
                .expect_err("absolute path")
        ),
        "PATH_ABSOLUTE"
    );
    assert!(Path::new(raw.as_ref()).is_absolute());
}

#[test]
fn pre_dispatch_policy_rejects_sensitive_paths_with_stable_codes() {
    let root = TempRoot::new("pre-dispatch");
    let policy = Policy::new(&root.path).expect("policy");

    let violation = policy
        .authorize(&AgentTool::WriteFile {
            path: ".ai-wiki/plans/task.md".into(),
            content: "not written".into(),
            expected_sha256: None,
        })
        .expect_err("harness-private write");

    assert_eq!(
        violation.to_string(),
        "[PATH_SENSITIVE] sensitive path is denied: .ai-wiki/plans/task.md"
    );
    assert!(matches!(
        violation,
        local_agent_harness::policy::PolicyViolation::Path(PathPolicyError::SensitivePath(_))
    ));
}
