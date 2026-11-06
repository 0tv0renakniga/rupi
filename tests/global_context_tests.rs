//! Global trusted context loading and provenance tests.

use std::path::{Path, PathBuf};

use local_agent_harness::config::Limits;
use local_agent_harness::context::GlobalContext;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rupi-global-context-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create temp directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directory");
        }
        std::fs::write(path, content).expect("write test file");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn limits() -> Limits {
    Limits {
        file_preview_bytes: 64,
        max_global_context_bytes: 256,
        ..Limits::default()
    }
}

#[test]
fn loads_fixed_and_prompt_sources_with_bounded_provenance() {
    let root = TempDir::new("sources");
    root.write("AGENTS.md", "operator preferences");
    root.write("CLAUDE.md", "organization conventions");
    root.write("prompts/zeta.md", "last template");
    root.write("prompts/alpha.md", "first template");
    root.write("prompts/ignored.txt", "not a prompt");
    root.write("prompts/nested/inside.md", "nested files are ignored");

    let context = GlobalContext::load(root.path(), &limits());

    assert!(!context.truncated);
    assert!(context.warnings.is_empty(), "{:?}", context.warnings);
    assert_eq!(context.sources.len(), 4);
    let paths = context
        .sources
        .iter()
        .map(|source| source.relative_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "AGENTS.md",
            "CLAUDE.md",
            "prompts/alpha.md",
            "prompts/zeta.md"
        ]
    );
    assert_eq!(context.sources[2].content, "first template");
    assert_eq!(
        context.total_bytes,
        context.sources.iter().map(|s| s.bytes).sum::<usize>()
    );

    let lines = context.display_lines().join("\n");
    assert!(lines.contains("AGENTS.md [trusted]"), "{lines}");
    assert!(lines.contains("prompts/alpha.md [trusted]"), "{lines}");
    assert!(lines.contains("operator preferences"), "{lines}");
    assert!(lines.contains("first template"), "{lines}");
}

#[test]
fn enforces_per_source_and_total_byte_limits() {
    let root = TempDir::new("limits");
    root.write("AGENTS.md", "abcdefghij");
    root.write("CLAUDE.md", "0123456789");
    root.write("prompts/overflow.md", "must not load");

    let context = GlobalContext::load(
        root.path(),
        &Limits {
            file_preview_bytes: 8,
            max_global_context_bytes: 10,
            ..Limits::default()
        },
    );

    assert!(context.truncated);
    assert_eq!(context.sources.len(), 2);
    assert_eq!(context.sources[0].content, "abcdefgh");
    assert_eq!(context.sources[0].bytes, 8);
    assert!(context.sources[0].truncated);
    assert_eq!(context.sources[1].content, "01");
    assert_eq!(context.sources[1].bytes, 2);
    assert!(context.sources[1].truncated);
    assert_eq!(context.total_bytes, 10);
    assert!(
        context
            .warnings
            .iter()
            .any(|warning| warning.contains("prompts/overflow.md"))
    );
}

#[test]
fn truncation_keeps_valid_utf8() {
    let root = TempDir::new("utf8");
    root.write("AGENTS.md", "ééé");

    let context = GlobalContext::load(
        root.path(),
        &Limits {
            file_preview_bytes: 3,
            max_global_context_bytes: 64,
            ..Limits::default()
        },
    );

    assert!(context.truncated);
    assert_eq!(context.sources.len(), 1);
    assert_eq!(context.sources[0].content, "é");
    assert_eq!(context.sources[0].bytes, 2);
}

#[test]
fn repository_content_is_not_scanned() {
    let config = TempDir::new("config");
    let project = TempDir::new("project");
    config.write("prompts/trusted.md", "trusted");
    project.write("AGENTS.md", "repository instructions");
    project.write("prompts/repository.md", "repository prompt");

    let context = GlobalContext::load(config.path(), &limits());

    assert_eq!(context.sources.len(), 1);
    assert_eq!(context.sources[0].relative_path, "prompts/trusted.md");
    let lines = context.display_lines().join("\n");
    assert!(!lines.contains("repository"), "{lines}");
}

#[cfg(unix)]
#[test]
fn symlinked_sources_and_prompt_directories_are_rejected() {
    use std::os::unix::fs::symlink;

    let config = TempDir::new("symlink-config");
    let project = TempDir::new("symlink-project");
    project.write("AGENTS.md", "repository instructions");
    project.write("repo-prompts/leak.md", "repository prompt");
    symlink(
        project.path().join("AGENTS.md"),
        config.path().join("AGENTS.md"),
    )
    .expect("create file symlink");
    symlink(
        project.path().join("repo-prompts"),
        config.path().join("prompts"),
    )
    .expect("create directory symlink");

    let context = GlobalContext::load(config.path(), &limits());

    assert!(context.sources.is_empty());
    assert!(
        context
            .warnings
            .iter()
            .any(|warning| warning.contains("AGENTS.md") && warning.contains("not a regular file"))
    );
    assert!(
        context
            .warnings
            .iter()
            .any(|warning| warning.contains("prompts")
                && warning.contains("not a regular directory"))
    );
}
