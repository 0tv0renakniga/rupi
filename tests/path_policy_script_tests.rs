use std::path::PathBuf;
use std::process::Command;

#[test]
fn python_path_policy_matches_the_shared_contract() {
    let python = python_interpreter();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(python)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "scripts/tests",
            "-p",
            "test_path_policy.py",
            "-v",
        ])
        .current_dir(&root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("run script path-policy tests");

    assert!(
        output.status.success(),
        "script path-policy tests failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn python_interpreter() -> PathBuf {
    for candidate in [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            for name in ["python3", "python3.exe", "python", "python.exe"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    panic!("python3 is required for script path-policy tests");
}
