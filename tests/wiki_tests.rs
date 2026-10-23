use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use local_agent_harness::error::AgentError;
use local_agent_harness::wiki::compile::{
    ActionSummary, SessionTrace, VerificationSummary, compile, derive_status,
};
use local_agent_harness::wiki::{
    REQUIRED_HEADINGS, WikiLimits, WikiStatus, load, parse_context, session_record_path, state_path,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "local-agent-harness-wiki-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temporary wiki root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn green_trace(id: &str) -> SessionTrace {
    let mut trace = SessionTrace::new(id, "complete the task", "2026-10-22T10:00:00Z");
    trace.verification.push(VerificationSummary {
        command: "cargo test".into(),
        exit_code: Some(0),
        required: true,
        summary: None,
    });
    trace
}

fn write_state(root: &Path, content: &str) {
    let path = state_path(root);
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("create wiki dir");
    std::fs::write(path, content).expect("write state");
}

fn assert_required_headings_once(body: &str) {
    for heading in REQUIRED_HEADINGS {
        assert_eq!(
            body.lines()
                .filter(|line| line.trim_end() == format!("## {heading}"))
                .count(),
            1,
            "missing or duplicate required heading {heading} in:\n{body}"
        );
    }
}

fn state_with(frontmatter: &str, body: &str) -> String {
    format!("---\n{frontmatter}\n---\n{body}")
}

#[tokio::test]
async fn missing_state_loads_empty_and_compile_creates_local_layout() {
    let root = TempDir::new();
    let context = load(root.path()).await.expect("missing state is valid");
    assert!(context.body.is_empty());
    assert_eq!(context.frontmatter.session_count, 0);
    assert!(context.sections.is_empty());

    let report = compile(root.path(), &context, &green_trace("s1"), "2026-10-22")
        .await
        .expect("compile empty state");

    assert!(state_path(root.path()).is_file());
    assert!(root.path().join(".ai-wiki").join("log.md").is_file());
    assert!(session_record_path(root.path()).is_file());
    assert!(root.path().join(".ai-wiki").join("sessions").is_dir());
    assert_eq!(report.session_count, 1);
    assert_eq!(report.status, WikiStatus::Green);

    let compiled = load(root.path()).await.expect("compiled state reloads");
    assert_required_headings_once(&compiled.body);
    assert_eq!(compiled.frontmatter.session_count, 1);
    assert_eq!(compiled.frontmatter.status, WikiStatus::Green);
}

#[tokio::test]
async fn valid_frontmatter_and_sections_are_typed() {
    let root = TempDir::new();
    let body = "## Current state\nready\n## Decisions\nchosen\n## Open items\nNone recorded.\n## How to verify\ncargo test\n";
    write_state(
        root.path(),
        &state_with(
            "schema_version: 1\ntitle: World\ntags:\n  - rust\n  - local\nupdated: 2026-10-22\nsession_count: 4\nstatus: green\nrisk_level: low\nlast_verified_commit: abc123\n",
            body,
        ),
    );

    let context = load(root.path()).await.expect("valid state");
    assert_eq!(context.frontmatter.title, "World");
    assert_eq!(context.frontmatter.tags, ["rust", "local"]);
    assert_eq!(context.frontmatter.session_count, 4);
    assert_eq!(context.frontmatter.status, WikiStatus::Green);
    assert_eq!(context.frontmatter.risk_level.as_deref(), Some("low"));
    assert_eq!(context.sections.len(), 4);
    assert_eq!(context.sections[0].heading, "Current state");
    assert_eq!(context.sections[0].content, "ready");
}

#[tokio::test]
async fn malformed_or_unclosed_frontmatter_is_a_wiki_error() {
    let root = TempDir::new();
    for content in [
        "---\ntitle: [unterminated\n---\nbody\n",
        "---\ntitle: World\nbody\n",
    ] {
        write_state(root.path(), content);
        let error = load(root.path()).await.expect_err("invalid frontmatter");
        assert!(matches!(error, AgentError::Wiki(_)), "{error}");
    }
}

#[tokio::test]
async fn input_limits_and_section_limits_are_enforced_without_splitting_utf8() {
    let source = Path::new("state.md");
    let content = "---\ntitle: World\n---\n## Current state\nbody\n";

    let state_limits = WikiLimits {
        max_state_bytes: 8,
        ..WikiLimits::default()
    };
    let error = parse_context(content, source, &state_limits).expect_err("state limit");
    assert!(matches!(error, AgentError::Wiki(_)), "{error}");

    let body_limits = WikiLimits {
        max_state_bytes: 4_096,
        max_body_bytes: 8,
        ..WikiLimits::default()
    };
    let error = parse_context(content, source, &body_limits).expect_err("body limit");
    assert!(matches!(error, AgentError::Wiki(_)), "{error}");

    let section_limits = WikiLimits {
        max_section_bytes: 40,
        max_sections: 1,
        ..WikiLimits::default()
    };
    let long_body = format!(
        "## Current state\n{}\n## Decisions\n{}\n",
        "é".repeat(60),
        "ignored second section"
    );
    let context = parse_context(&long_body, source, &section_limits).expect("section limits");
    assert_eq!(context.sections.len(), 1);
    assert_eq!(context.sections[0].heading, "Current state");
    assert!(context.sections[0].byte_len <= 40);
    assert_eq!(
        context.sections[0].byte_len,
        context.sections[0].content.len()
    );
    assert!(context.sections[0].content.contains("truncated"));
    assert!(std::str::from_utf8(context.sections[0].content.as_bytes()).is_ok());
}

#[tokio::test]
async fn green_status_requires_passing_required_verification() {
    let root = TempDir::new();
    let report = compile(
        root.path(),
        &load(root.path()).await.unwrap(),
        &green_trace("green"),
        "2026-10-22",
    )
    .await
    .expect("compile green status");
    assert_eq!(report.status, WikiStatus::Green);

    let state = load(root.path()).await.expect("reload state");
    assert_eq!(state.frontmatter.status, WikiStatus::Green);
    assert!(state.body.contains("PASS `cargo test`"));
}

#[tokio::test]
async fn any_recorded_verification_failure_is_red() {
    let root = TempDir::new();
    let mut trace = green_trace("red");
    trace.verification.push(VerificationSummary {
        command: "cargo clippy".into(),
        exit_code: Some(1),
        required: false,
        summary: Some("warning denied".into()),
    });

    let report = compile(
        root.path(),
        &load(root.path()).await.unwrap(),
        &trace,
        "2026-10-22",
    )
    .await
    .expect("compile red status");
    assert_eq!(report.status, WikiStatus::Red);

    let state = load(root.path()).await.expect("reload state");
    assert_eq!(state.frontmatter.status, WikiStatus::Red);
    assert!(state.body.contains("FAIL `cargo clippy` (exit 1)"));
}

#[tokio::test]
async fn missing_unavailable_denied_blocked_and_open_item_states_are_blocked() {
    let cases = [
        ("missing required", Vec::new()),
        (
            "unavailable required",
            vec![VerificationSummary {
                command: "cargo test".into(),
                exit_code: None,
                required: true,
                summary: None,
            }],
        ),
    ];

    for (name, verification) in cases {
        let root = TempDir::new();
        let mut trace = SessionTrace::new(name, "task", "now");
        trace.verification = verification;
        assert_eq!(derive_status(&trace), WikiStatus::Blocked, "{name}");
        let report = compile(
            root.path(),
            &load(root.path()).await.unwrap(),
            &trace,
            "2026-10-22",
        )
        .await
        .expect("compile blocked status");
        assert_eq!(report.status, WikiStatus::Blocked, "{name}");
    }

    for (name, configure) in [
        (
            "policy denial",
            (|trace: &mut SessionTrace| trace.policy_denials.push("denied rm".into()))
                as fn(&mut SessionTrace),
        ),
        ("blocker", |trace: &mut SessionTrace| {
            trace.blockers.push("cannot proceed".into())
        }),
        ("open item", |trace: &mut SessionTrace| {
            trace.open_items.push("finish docs".into())
        }),
    ] {
        let root = TempDir::new();
        let mut trace = green_trace(name);
        configure(&mut trace);
        assert_eq!(derive_status(&trace), WikiStatus::Blocked, "{name}");
        let report = compile(
            root.path(),
            &load(root.path()).await.unwrap(),
            &trace,
            "2026-10-22",
        )
        .await
        .expect("compile blocked status");
        assert_eq!(report.status, WikiStatus::Blocked, "{name}");
        let state = load(root.path()).await.expect("reload state");
        assert_eq!(state.frontmatter.status, WikiStatus::Blocked, "{name}");
    }
}

#[tokio::test]
async fn prior_open_items_keep_a_green_trace_blocked() {
    let root = TempDir::new();
    let initial = state_with(
        "title: Open work\nsession_count: 1\nstatus: blocked\n",
        "## Current state\nready\n## Open items\n- unresolved migration\n",
    );
    write_state(root.path(), &initial);

    let report = compile(
        root.path(),
        &load(root.path()).await.unwrap(),
        &green_trace("open"),
        "2026-10-22",
    )
    .await
    .expect("compile with prior open item");
    assert_eq!(report.status, WikiStatus::Blocked);

    let state = load(root.path()).await.expect("reload state");
    assert_eq!(state.frontmatter.status, WikiStatus::Blocked);
    assert!(state.body.contains("unresolved migration"));
}

#[tokio::test]
async fn compile_increments_sessions_preserves_bounded_context_and_appends_history() {
    let root = TempDir::new();
    let initial = state_with(
        "schema_version: 1\ntitle: Inherited\ntags:\n  - rust\nupdated: 2026-10-01\nsession_count: 7\nstatus: blocked\n",
        "## Current state\nprior context marker\n## Decisions\nprior decision marker\n## Open items\nNone recorded.\n## How to verify\ncargo test\n",
    );
    write_state(root.path(), &initial);

    let first_prior = load(root.path()).await.expect("load prior state");
    let first = compile(
        root.path(),
        &first_prior,
        &green_trace("session-1"),
        "2026-10-22",
    )
    .await
    .expect("first compile");
    assert_eq!(first.session_count, 8);
    let log_after_first = std::fs::read_to_string(root.path().join(".ai-wiki").join("log.md"))
        .expect("read log after first");
    let record_after_first = std::fs::read_to_string(session_record_path(root.path()))
        .expect("read session record after first");
    assert_eq!(log_after_first.lines().count(), 1);
    assert_eq!(record_after_first.lines().count(), 1);

    let second_prior = load(root.path()).await.expect("load first compiled state");
    let second = compile(
        root.path(),
        &second_prior,
        &green_trace("session-2"),
        "2026-10-23",
    )
    .await
    .expect("second compile");
    assert_eq!(second.session_count, 9);

    let state = load(root.path()).await.expect("load second compiled state");
    assert_eq!(state.frontmatter.session_count, 9);
    assert_eq!(state.frontmatter.title, "Inherited");
    assert_eq!(state.frontmatter.tags, ["rust"]);
    assert!(state.body.contains("prior context marker"));
    assert!(state.body.contains("prior decision marker"));

    let log_after_second = std::fs::read_to_string(root.path().join(".ai-wiki").join("log.md"))
        .expect("read log after second");
    let record_after_second = std::fs::read_to_string(session_record_path(root.path()))
        .expect("read session record after second");
    assert!(log_after_second.starts_with(&log_after_first));
    assert!(record_after_second.starts_with(&record_after_first));
    assert_eq!(log_after_second.lines().count(), 2);
    assert_eq!(record_after_second.lines().count(), 2);

    for line in record_after_second.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("valid JSONL record");
        assert!(value["session_id"].is_string());
    }
}

#[tokio::test]
async fn rendered_state_log_and_session_records_are_redacted() {
    let root = TempDir::new();
    let initial = state_with(
        "title: Redaction\nsession_count: 0\nstatus: blocked\n",
        "## Current state\nprior KEY=priorsecretvalue\n",
    );
    write_state(root.path(), &initial);

    let mut trace = SessionTrace::new(
        "session-redaction",
        "deploy KEY=supersecretvalue and sk-proj-abcdefghijklmnopqrstuvwxyz",
        "started TOKEN=abc123",
    );
    trace.finished_at = Some("Authorization: Bearer zyxwvutsrqponmlk".into());
    trace
        .changed_files
        .insert("sk-proj-abcdefghijklmnopqrstuvwxyz".into());
    trace.actions.push(ActionSummary {
        tool: "shell".into(),
        summary: "PASSWORD=hunter2".into(),
        ok: false,
    });
    trace.decisions.push("Bearer abcdefghijklmnop".into());
    trace.policy_denials.push("SECRET: policysecret".into());
    trace
        .blockers
        .push("postgres://user:dbpassword@example.com/db".into());
    trace.open_items.push("password=hunter3".into());
    trace.verification.push(VerificationSummary {
        command: "cargo test".into(),
        exit_code: Some(0),
        required: true,
        summary: Some("KEY=verifysecret".into()),
    });

    compile(
        root.path(),
        &load(root.path()).await.unwrap(),
        &trace,
        "2026-10-22",
    )
    .await
    .expect("compile redacted artifacts");

    let state = std::fs::read_to_string(state_path(root.path())).expect("read state");
    let log =
        std::fs::read_to_string(root.path().join(".ai-wiki").join("log.md")).expect("read log");
    let records = std::fs::read_to_string(session_record_path(root.path())).expect("read sessions");
    let combined = format!("{state}\n{log}\n{records}");

    for secret in [
        "priorsecretvalue",
        "supersecretvalue",
        "sk-proj-abcdefghijklmnopqrstuvwxyz",
        "abcdefghijklmnop",
        "zyxwvutsrqponmlk",
        "hunter2",
        "hunter3",
        "dbpassword",
        "policysecret",
        "verifysecret",
        "abc123",
    ] {
        assert!(!combined.contains(secret), "{secret} leaked in {combined}");
    }
    assert!(combined.contains("[REDACTED]") || combined.contains("[REDACTED_SECRET]"));

    assert_required_headings_once(
        &load(root.path())
            .await
            .expect("redacted state reloads")
            .body,
    );
    let record: serde_json::Value =
        serde_json::from_str(records.lines().next().expect("session line")).expect("JSON trace");
    assert!(!record.to_string().contains("hunter2"));
}

#[tokio::test]
async fn state_writes_are_atomic_and_leave_no_temporary_artifacts() {
    let root = TempDir::new();
    let initial = state_with(
        "title: Atomic\nsession_count: 1\nstatus: blocked\n",
        "## Current state\nold state\n",
    );
    write_state(root.path(), &initial);

    compile(
        root.path(),
        &load(root.path()).await.unwrap(),
        &green_trace("atomic"),
        "2026-10-22",
    )
    .await
    .expect("atomic compile");

    let names = std::fs::read_dir(root.path().join(".ai-wiki"))
        .expect("read wiki directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        names.iter().all(|name| !name.contains(".tmp-")),
        "temporary artifacts remain: {names:?}"
    );
    let state = std::fs::read_to_string(state_path(root.path())).expect("read state");
    assert!(state.contains("old state"));
    assert_required_headings_once(&state);
    assert_required_headings_once(&load(root.path()).await.expect("reload").body);
}

#[test]
fn wiki_production_source_has_no_network_telemetry_or_remote_storage_code() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wiki");
    let forbidden = [
        "reqwest",
        "hyper",
        "std::net",
        "tokio::net",
        "TcpStream",
        "UdpSocket",
        "telemetry",
        "analytics",
        "http://",
        "https://",
        "sqlite",
        "rusqlite",
        "dynamodb",
        "firestore",
        "mongodb",
        "s3://",
    ];

    for file in ["mod.rs", "redact.rs", "compile.rs"] {
        let source = std::fs::read_to_string(source_root.join(file)).expect("read wiki source");
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        for forbidden in forbidden {
            assert!(
                !production.contains(forbidden),
                "{file} contains forbidden production token {forbidden}"
            );
        }
    }
}
