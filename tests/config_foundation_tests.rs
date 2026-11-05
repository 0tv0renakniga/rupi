use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use local_agent_harness::config::Config;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn config_load_uses_trusted_file_and_environment_overrides() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let layout = Layout::new("load");
    layout.write_config(
        r#"
root = "{{PROJECT}}"
model = "config-model"
max_act_turns = 7
max_verify_retries = 2

[limits]
context_cap_tokens = 8192
reserved_output_tokens = 1024
file_preview_bytes = 2048
max_search_results = 25
max_tool_output_bytes = 4096
max_session_event_bytes = 32768
"#,
    );

    let _guard = EnvGuard::new(&[
        (
            "HARNESS_CONFIG_DIR",
            Some(layout.config_dir.as_os_str().to_owned()),
        ),
        ("HARNESS_ROOT", Some(layout.project.as_os_str().to_owned())),
        ("HARNESS_MODEL", Some("env-model".into())),
        ("HARNESS_MAX_ACT_TURNS", Some("11".into())),
        ("HARNESS_CONTEXT_CAP_TOKENS", Some("12288".into())),
        (
            "XDG_DATA_HOME",
            Some(layout.data_dir.as_os_str().to_owned()),
        ),
        ("XDG_CONFIG_HOME", Some(layout.base.as_os_str().to_owned())),
        ("HARNESS_SCRIPT_ROOT", Some(scripts_dir().into_os_string())),
        (
            "HARNESS_SCRIPT_INTERPRETER",
            Some(python_interpreter().into_os_string()),
        ),
        ("HARNESS_ENDPOINT_PROFILE", Some("local".into())),
        ("HARNESS_LLM_BASE", Some("http://127.0.0.1:8080".into())),
        ("HARNESS_LLM_ALLOWED_ORIGINS", None),
        ("HARNESS_LLM_CREDENTIAL_ENV", None),
        ("HARNESS_AUDIT_PATH", None),
        ("HARNESS_MAX_TOOL_OUTPUT_BYTES", None),
    ]);

    let config = Config::load().expect("load trusted configuration");
    config.validate().expect("configuration validates");

    assert_eq!(config.root, layout.project);
    assert_eq!(config.model, "env-model");
    assert_eq!(config.max_act_turns(), 11);
    assert_eq!(config.max_verify_retries(), 2);
    assert_eq!(config.limits.context_cap_tokens, 12_288);
    assert_eq!(config.limits.reserved_output_tokens, 1_024);
    assert_eq!(config.limits.file_preview_bytes, 2_048);
    assert_eq!(config.limits.max_search_results, 25);
    assert_eq!(config.trusted_config_dir, layout.config_dir);
    assert_eq!(
        config.session_data_dir,
        layout.data_dir.join("local-agent-harness/sessions")
    );
}

#[test]
fn config_load_does_not_scan_the_repository_for_configuration() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let layout = Layout::new("ignored");
    std::fs::write(
        layout.project.join("config.toml"),
        "model = \"repo-model\"\n",
    )
    .expect("write repository config");

    let _guard = EnvGuard::new(&[
        (
            "HARNESS_CONFIG_DIR",
            Some(layout.config_dir.as_os_str().to_owned()),
        ),
        ("HARNESS_ROOT", Some(layout.project.as_os_str().to_owned())),
        ("HARNESS_MODEL", None),
        ("HARNESS_MAX_ACT_TURNS", None),
        ("HARNESS_CONTEXT_CAP_TOKENS", None),
        (
            "XDG_DATA_HOME",
            Some(layout.data_dir.as_os_str().to_owned()),
        ),
        ("XDG_CONFIG_HOME", Some(layout.base.as_os_str().to_owned())),
        ("HARNESS_SCRIPT_ROOT", Some(scripts_dir().into_os_string())),
        (
            "HARNESS_SCRIPT_INTERPRETER",
            Some(python_interpreter().into_os_string()),
        ),
        ("HARNESS_ENDPOINT_PROFILE", None),
        ("HARNESS_LLM_BASE", None),
        ("HARNESS_LLM_ALLOWED_ORIGINS", None),
        ("HARNESS_LLM_CREDENTIAL_ENV", None),
        ("HARNESS_AUDIT_PATH", None),
    ]);

    let config = Config::load().expect("load trusted configuration");
    config.validate().expect("configuration validates");
    assert_eq!(config.model, "local-model");
    assert_ne!(config.model, "repo-model");
    assert_eq!(config.trusted_config_dir, layout.config_dir);
}

struct Layout {
    base: PathBuf,
    project: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl Layout {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "rupi-config-foundation-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        let project = base.join("project");
        let config_dir = base.join("trusted-config");
        let data_dir = base.join("data");
        std::fs::create_dir_all(&project).expect("create project");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        Self {
            base,
            project,
            config_dir,
            data_dir,
        }
    }

    fn write_config(&self, template: &str) {
        let raw = template.replace("{{PROJECT}}", &self.project.display().to_string());
        std::fs::write(self.config_dir.join("config.toml"), raw).expect("write config.toml");
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn python_interpreter() -> PathBuf {
    for candidate in [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
    ] {
        let path = Path::new(candidate);
        if path.is_file() {
            return path.to_path_buf();
        }
    }
    panic!("python3 is required for config foundation tests");
}

struct EnvGuard {
    saved: Vec<(String, Option<OsString>)>,
}

impl EnvGuard {
    fn new(values: &[(&str, Option<OsString>)]) -> Self {
        let mut saved = Vec::new();
        for (name, value) in values {
            saved.push(((*name).to_owned(), std::env::var_os(name)));
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
