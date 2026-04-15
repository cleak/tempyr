use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::config::ProjectContext;

use super::git_hooks;
use super::index_cmd;
use super::managed::{self, ManagedArtifact, WriteOutcome};
use super::onboarding::{
    self, EmbeddingProviderChoice, ExistingDocMode, ExistingDocs, OnboardingSelections,
};
use tempyr_core::project;
use tempyr_index::embeddings;

const DEFAULT_SCHEMA: &str = include_str!("../../../../schema/default-schema.toml");
const CLAUDE_DOC_TEMPLATE: &str = include_str!("../../assets/CLAUDE.template.md");
const AGENTS_DOC_TEMPLATE: &str = include_str!("../../assets/AGENTS.template.md");
const CODEX_SKILL_TEMPLATE: &str = include_str!("../../assets/tempyr-interview.codex.SKILL.md");
const PRD_TEMPLATE: &str = include_str!("../../../../templates/prd.toml");
const TDD_TEMPLATE: &str = include_str!("../../../../templates/tdd.toml");
const TASK_PROMPT_TEMPLATE: &str = include_str!("../../../../templates/task-prompt.toml");

pub fn run(json_output: bool, force_wizard: bool, no_wizard: bool) -> anyhow::Result<()> {
    if json_output && force_wizard {
        anyhow::bail!("--json cannot be combined with --wizard");
    }
    if json_output {
        anyhow::bail!("`tempyr init --json` is not supported yet");
    }

    let cwd = std::env::current_dir()?;
    let tempyr_dir = cwd.join(".tempyr");
    if tempyr_dir.exists() {
        anyhow::bail!("Already initialized: .tempyr/ exists");
    }

    let existing_docs = detect_existing_docs(&cwd);
    let selections = if should_launch_wizard(force_wizard, no_wizard) {
        onboarding::run(existing_docs)?
            .ok_or_else(|| anyhow::anyhow!("Initialization cancelled."))?
    } else {
        noninteractive_defaults(existing_docs)
    };

    initialize_project(&cwd, &selections)
}

fn should_launch_wizard(force_wizard: bool, no_wizard: bool) -> bool {
    if no_wizard {
        return false;
    }
    if force_wizard {
        return true;
    }
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

fn noninteractive_defaults(existing_docs: ExistingDocs) -> OnboardingSelections {
    OnboardingSelections {
        provider: EmbeddingProviderChoice::Voyage,
        api_key: None,
        write_api_key_to_env_local: false,
        create_env_local_from_template: false,
        validate_provider_setup: false,
        run_index_rebuild: false,
        install_render_overrides: false,
        install_claude_hooks: true,
        install_claude_skill: true,
        install_claude_agent: true,
        install_claude_doc: false,
        install_codex_skill: false,
        install_codex_doc: false,
        write_mcp_setup_notes: false,
        existing_doc_mode: if existing_docs.any() {
            ExistingDocMode::Manual
        } else {
            ExistingDocMode::Append
        },
    }
}

fn detect_existing_docs(root: &Path) -> ExistingDocs {
    ExistingDocs {
        claude_md: root.join("CLAUDE.md").is_file(),
        agents_md: root.join("AGENTS.md").is_file(),
    }
}

fn initialize_project(root: &Path, selections: &OnboardingSelections) -> anyhow::Result<()> {
    let tempyr_dir = root.join(".tempyr");
    let graph_dir = root.join("graph");

    fs::create_dir_all(&tempyr_dir)?;
    fs::create_dir_all(tempyr_dir.join("render"))?;
    fs::create_dir_all(tempyr_dir.join("sessions"))?;

    fs::write(tempyr_dir.join("schema.toml"), DEFAULT_SCHEMA)?;
    fs::write(
        tempyr_dir.join("config.toml"),
        render_config(selections.provider),
    )?;

    let schema: tempyr_core::schema::Schema = DEFAULT_SCHEMA.parse()?;
    for node_type in schema.node_types.values() {
        fs::create_dir_all(graph_dir.join(&node_type.directory))?;
    }

    let mut summary = vec![
        "Initialized tempyr project in".to_string(),
        format!("  {}", root.display()),
        "  .tempyr/schema.toml  - node and edge type definitions".to_string(),
        format!(
            "  .tempyr/config.toml  - project configuration ({})",
            selections.provider.label()
        ),
        "  graph/               - node files organized by type".to_string(),
    ];

    if selections.create_env_local_from_template {
        let outcome = ensure_env_local_from_template(root)?;
        if !matches!(outcome, SupportWriteOutcome::Unchanged) {
            summary.push(format!(
                "  .env.local           - {}",
                outcome.label("created from template")
            ));
        }
        ensure_gitignore_contains(root, ".env.local")?;
    }

    if selections.write_api_key_to_env_local
        && let Some(key) = selections.api_key.as_deref()
    {
        let env_var = write_provider_api_key(root, selections.provider, key)?;
        summary.push(format!("  .env.local           - stored {}", env_var));
    }

    let claude_artifacts = selected_claude_artifacts(selections);
    if !claude_artifacts.is_empty() {
        let results = managed::install_selected(root, false, &claude_artifacts)?;
        summary.extend(render_managed_results(&results));
    }

    if selections.install_codex_skill {
        let outcome = write_support_file(
            &root.join(".agents/skills/tempyr-interview/SKILL.md"),
            CODEX_SKILL_TEMPLATE,
        )?;
        summary.push(format!(
            "  .agents/skills/tempyr-interview/SKILL.md - {}",
            outcome.label("repo-local Codex skill")
        ));
    }

    if selections.install_render_overrides {
        summary.extend(install_render_overrides(root)?);
    }

    if selections.write_mcp_setup_notes {
        let path = write_mcp_setup_notes(root)?;
        summary.push(format!(
            "  {}  - MCP setup notes",
            display_relative(root, &path)
        ));
    }

    let git_hook_results = git_hooks::install_all(root)?;
    summary.extend(render_git_hook_results(&git_hook_results));

    let mut existing_doc_updates = Vec::new();

    if selections.install_claude_doc {
        handle_doc_target(
            root,
            &mut summary,
            &mut existing_doc_updates,
            DocSpec::new("CLAUDE.md", CLAUDE_DOC_TEMPLATE, "Claude Code instructions"),
        )?;
    }

    if selections.install_codex_doc {
        handle_doc_target(
            root,
            &mut summary,
            &mut existing_doc_updates,
            DocSpec::new(
                "AGENTS.md",
                AGENTS_DOC_TEMPLATE,
                "Codex / agent instructions",
            ),
        )?;
    }

    if !existing_doc_updates.is_empty() {
        match selections.existing_doc_mode {
            ExistingDocMode::Append => {
                for spec in &existing_doc_updates {
                    append_doc_section(&root.join(spec.path), spec.section())?;
                    summary.push(format!(
                        "  {}               - appended Tempyr guidance",
                        spec.path
                    ));
                }
            }
            ExistingDocMode::Manual => {
                let path = write_doc_follow_up(root, &existing_doc_updates, FollowUpMode::Manual)?;
                summary.push(format!(
                    "  {}  - manual merge instructions",
                    display_relative(root, &path)
                ));
            }
            ExistingDocMode::ClaudeCode => {
                let path =
                    write_doc_follow_up(root, &existing_doc_updates, FollowUpMode::ClaudeCode)?;
                summary.push(format!(
                    "  {}  - Claude Code handoff prompt",
                    display_relative(root, &path)
                ));
            }
            ExistingDocMode::Codex => {
                let path = write_doc_follow_up(root, &existing_doc_updates, FollowUpMode::Codex)?;
                summary.push(format!(
                    "  {}  - Codex handoff prompt",
                    display_relative(root, &path)
                ));
            }
        }
    }

    if selections.validate_provider_setup {
        match validate_provider_setup(root) {
            Ok(message) => summary.push(format!("  provider validation  - {message}")),
            Err(err) => summary.push(format!("  provider validation  - warning: {err}")),
        }
    }

    println!("{}", summary.join("\n"));

    if selections.run_index_rebuild {
        let ctx = ProjectContext::find(Some(root.join("graph").as_path()))?;
        println!();
        println!("Running initial index rebuild...");
        if let Err(err) = index_cmd::run_rebuild(&ctx, false, false) {
            eprintln!("Warning: initial index rebuild failed: {err}");
        }
    }

    Ok(())
}

fn handle_doc_target(
    root: &Path,
    summary: &mut Vec<String>,
    existing_doc_updates: &mut Vec<DocSpec>,
    spec: DocSpec,
) -> anyhow::Result<()> {
    let path = root.join(spec.path);
    if path.exists() {
        existing_doc_updates.push(spec);
        return Ok(());
    }

    let outcome = write_support_file(&path, spec.template)?;
    summary.push(format!(
        "  {}               - {}",
        spec.path,
        outcome.label(spec.description)
    ));
    Ok(())
}

fn render_managed_results(results: &[managed::InstallResult]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| match result.outcome {
            WriteOutcome::Created => Some(format!("  {:<23}- {}", result.path, result.description)),
            WriteOutcome::Merged => Some(format!("  {:<23}- {}", result.path, result.description)),
            WriteOutcome::Updated => Some(format!("  {:<23}- {}", result.path, result.description)),
            WriteOutcome::Skipped => {
                Some(format!("  {:<23}- skipped (user modified)", result.path))
            }
            WriteOutcome::Unchanged => None,
        })
        .collect()
}

fn render_git_hook_results(results: &[git_hooks::HookInstallResult]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| match result.outcome {
            WriteOutcome::Created => Some(format!(
                "  git hook {:<14}- {}",
                result.name, result.description
            )),
            WriteOutcome::Merged => Some(format!(
                "  git hook {:<14}- {}",
                result.name, result.description
            )),
            WriteOutcome::Updated => Some(format!(
                "  git hook {:<14}- {}",
                result.name, result.description
            )),
            WriteOutcome::Skipped => None,
            WriteOutcome::Unchanged => None,
        })
        .collect()
}

fn selected_claude_artifacts(selections: &OnboardingSelections) -> Vec<ManagedArtifact> {
    let mut artifacts = Vec::new();
    if selections.install_claude_hooks {
        artifacts.push(ManagedArtifact::Hooks);
    }
    if selections.install_claude_skill {
        artifacts.push(ManagedArtifact::Skill);
    }
    if selections.install_claude_agent {
        artifacts.push(ManagedArtifact::Agent);
    }
    artifacts
}

fn render_config(provider: EmbeddingProviderChoice) -> String {
    let (provider_name, model, dimensions, key_comment) = match provider {
        EmbeddingProviderChoice::Voyage => (
            "voyage",
            "voyage-4",
            1024,
            "# API key: set VOYAGE_API_KEY in .env.local or your shell environment",
        ),
        EmbeddingProviderChoice::Gemini => (
            "gemini",
            "gemini-embedding-001",
            768,
            "# API key: set GEMINI_API_KEY in .env.local or your shell environment",
        ),
        EmbeddingProviderChoice::Local => (
            "local",
            "all-MiniLM-L6-v2",
            384,
            "# No API key required. Local embeddings require building with --features local-embeddings",
        ),
    };

    format!(
        r#"[general]
graph_dir = "graph"
schema_path = ".tempyr/schema.toml"

[embedding]
provider = "{provider_name}"          # voyage | gemini | local
model = "{model}"
dimensions = {dimensions}
{key_comment}

[llm]
provider = "anthropic"
model = "claude-opus-4-6"
temperature = 0.1

[retrieval]
default_token_budget = 8000
structural_weight = 0.5
bm25_weight = 0.25
vector_weight = 0.25
recency_boost_days = 7
recency_boost_value = 0.1

[interview]
max_questions_per_turn = 3
auto_advance_phases = true
session_timeout_hours = 168

[mcp]
transport = "stdio"
"#
    )
}

fn upsert_env_var(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let mut lines = if path.exists() {
        fs::read_to_string(path)?
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with(&format!("{key}=")) {
            *line = format!("{key}={value}");
            replaced = true;
        }
    }

    if !replaced {
        lines.push(format!("{key}={value}"));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }
    fs::write(path, output)?;
    Ok(())
}

fn write_provider_api_key(
    root: &Path,
    provider: EmbeddingProviderChoice,
    key: &str,
) -> anyhow::Result<&'static str> {
    let env_var = provider
        .env_var()
        .ok_or_else(|| anyhow::anyhow!("Selected provider does not use an API key"))?;
    embeddings::validate_api_key_value(env_var, key)?;
    upsert_env_var(&root.join(".env.local"), env_var, key)?;
    ensure_gitignore_contains(root, ".env.local")?;
    Ok(env_var)
}

fn ensure_gitignore_contains(root: &Path, entry: &str) -> anyhow::Result<()> {
    let path = root.join(".gitignore");
    let existing = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };

    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }

    let mut output = existing;
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str("# Tempyr local secrets\n");
    output.push_str(entry);
    output.push('\n');

    fs::write(path, output)?;
    Ok(())
}

fn ensure_env_local_from_template(root: &Path) -> anyhow::Result<SupportWriteOutcome> {
    let path = root.join(".env.local");
    if path.exists() {
        return Ok(SupportWriteOutcome::Unchanged);
    }

    let content = if root.join(".env.example").exists() {
        fs::read_to_string(root.join(".env.example"))?
    } else {
        String::new()
    };

    fs::write(path, content)?;
    Ok(SupportWriteOutcome::Created)
}

fn install_render_overrides(root: &Path) -> anyhow::Result<Vec<String>> {
    let render_dir = root.join(".tempyr").join("render");
    fs::create_dir_all(&render_dir)?;

    let mut messages = Vec::new();
    for (filename, content) in [
        ("prd.toml", PRD_TEMPLATE),
        ("tdd.toml", TDD_TEMPLATE),
        ("task-prompt.toml", TASK_PROMPT_TEMPLATE),
    ] {
        let outcome = write_support_file(&render_dir.join(filename), content)?;
        messages.push(format!(
            "  .tempyr/render/{filename} - {}",
            outcome.label("local render override")
        ));
    }

    Ok(messages)
}

fn write_mcp_setup_notes(root: &Path) -> anyhow::Result<PathBuf> {
    let onboarding_dir = root.join(".tempyr").join("onboarding");
    fs::create_dir_all(&onboarding_dir)?;
    let path = onboarding_dir.join("mcp-setup.md");
    let body = r#"# Tempyr MCP Setup

Register a stdio MCP server named `tempyr` that runs:

```text
tempyr --mcp
```

## Claude Code

- Prefer running Claude from the repo root.
- Use project-level MCP configuration or a launch-time `--mcp-config` entry that starts `tempyr --mcp`.
- If you want Claude to merge existing instruction docs, prefer `--permission-mode acceptEdits` with narrow `Edit(...)` tool rules for the target markdown files.

## Codex

- Configure a project MCP server entry that starts `tempyr --mcp`.
- If you want Codex to update existing instruction docs, use project config with narrow writable roots for those markdown files.
- Repo-local `.codex` and `.agents` paths can remain protected even when writable roots are restricted, so Tempyr installs supporting assets directly and limits merge handoffs to markdown docs.
"#;
    fs::write(&path, body)?;
    Ok(path)
}

fn validate_provider_setup(root: &Path) -> anyhow::Result<String> {
    let _loaded = project::load_project_env_from(root.to_path_buf())?;
    let ctx = ProjectContext::find(Some(root.join("graph").as_path()))?;
    let resolved = ctx.resolved_embedding_config()?;
    if let Some(env_var) = embeddings::provider_api_key_env_var(&resolved.provider)
        && let Ok(value) = std::env::var(env_var)
    {
        embeddings::validate_api_key_value(env_var, &value)?;
    }
    let provider = embeddings::create_provider_from_resolved(&resolved)?;
    Ok(format!(
        "{} provider ready ({})",
        resolved.provider,
        provider.name()
    ))
}

fn write_support_file(path: &Path, content: &str) -> anyhow::Result<SupportWriteOutcome> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if existing == content {
            return Ok(SupportWriteOutcome::Unchanged);
        }
        return Ok(SupportWriteOutcome::SkippedModified);
    }

    fs::write(path, content)?;
    Ok(SupportWriteOutcome::Created)
}

fn append_doc_section(path: &Path, section: String) -> anyhow::Result<()> {
    let begin_marker = "<!-- tempyr:onboarding:start -->";
    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if existing.contains(begin_marker) {
            return Ok(());
        }

        let mut output = existing;
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
        output.push_str(begin_marker);
        output.push('\n');
        output.push_str(&section);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("<!-- tempyr:onboarding:end -->\n");
        fs::write(path, output)?;
        return Ok(());
    }

    fs::write(
        path,
        format!("{begin_marker}\n{section}\n<!-- tempyr:onboarding:end -->\n"),
    )?;
    Ok(())
}

fn write_doc_follow_up(
    root: &Path,
    docs: &[DocSpec],
    mode: FollowUpMode,
) -> anyhow::Result<PathBuf> {
    let onboarding_dir = root.join(".tempyr").join("onboarding");
    fs::create_dir_all(&onboarding_dir)?;

    let filename = match mode {
        FollowUpMode::Manual => "manual-doc-update.md",
        FollowUpMode::ClaudeCode => "claude-code-doc-update.md",
        FollowUpMode::Codex => "codex-doc-update.md",
    };
    let path = onboarding_dir.join(filename);
    fs::write(&path, render_follow_up_body(docs, mode))?;
    Ok(path)
}

fn render_follow_up_body(docs: &[DocSpec], mode: FollowUpMode) -> String {
    let title = match mode {
        FollowUpMode::Manual => "# Manual Tempyr Doc Update\n\n",
        FollowUpMode::ClaudeCode => "# Claude Code Tempyr Doc Update\n\n",
        FollowUpMode::Codex => "# Codex Tempyr Doc Update\n\n",
    };

    let mut body = String::from(title);
    body.push_str("Merge the Tempyr guidance into these existing instruction files without removing project-specific content.\n\n");

    match mode {
        FollowUpMode::Manual => {
            body.push_str("Suggested approach:\n");
            body.push_str("- Keep existing repository guidance intact.\n");
            body.push_str("- Add the Tempyr section where it best fits.\n");
            body.push_str(
                "- Prefer one Tempyr section per file rather than duplicating content.\n\n",
            );
        }
        FollowUpMode::ClaudeCode => {
            body.push_str("Suggested Claude Code launch pattern:\n");
            body.push_str("- Run Claude from the repo root.\n");
            body.push_str("- Use `--permission-mode acceptEdits`.\n");
            body.push_str("- Prefer `--allowedTools Read,Grep,Glob,Edit(/CLAUDE.md),Edit(/AGENTS.md)` to narrow writes to the instruction docs you want merged.\n");
            body.push_str("- Supporting `.claude` hooks/skills were installed directly by Tempyr because protected directories can still prompt.\n\n");
        }
        FollowUpMode::Codex => {
            body.push_str("Suggested Codex setup notes:\n");
            body.push_str("- Use project-scoped `.codex/config.toml` with `sandbox_mode`, `approval_policy`, and `sandbox_workspace_write.writable_roots` tuned to the doc files you want updated.\n");
            body.push_str("- OpenAI's docs note that repo-local `.codex` and `.agents` paths remain protected inside default writable roots, so Tempyr installs supporting skill files directly and limits Codex handoff to markdown docs.\n");
            body.push_str("- After opening Codex in the repo, point it at this file and ask it to merge the snippets below.\n\n");
        }
    }

    for doc in docs {
        body.push_str(&format!("## {}\n\n", doc.path));
        body.push_str("```md\n");
        body.push_str(&doc.section());
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n\n");
    }

    body
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug, Clone, Copy)]
struct DocSpec {
    path: &'static str,
    template: &'static str,
    description: &'static str,
}

impl DocSpec {
    const fn new(path: &'static str, template: &'static str, description: &'static str) -> Self {
        Self {
            path,
            template,
            description,
        }
    }

    fn section(&self) -> String {
        markdown_section(self.template)
    }
}

#[derive(Debug, Clone, Copy)]
enum FollowUpMode {
    Manual,
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupportWriteOutcome {
    Created,
    Unchanged,
    SkippedModified,
}

impl SupportWriteOutcome {
    fn label(self, description: &str) -> String {
        match self {
            Self::Created => description.to_string(),
            Self::Unchanged => format!("{description} (already up to date)"),
            Self::SkippedModified => format!("{description} (skipped, file already exists)"),
        }
    }
}

fn markdown_section(full_doc: &str) -> String {
    let mut lines = full_doc.lines();
    let first = lines.next().unwrap_or("# Tempyr Knowledge Graph");
    let heading = first.trim_start_matches('#').trim();
    let remainder = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    if remainder.is_empty() {
        format!("## {heading}")
    } else {
        format!("## {heading}\n\n{remainder}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_config_uses_provider_defaults() {
        let voyage = render_config(EmbeddingProviderChoice::Voyage);
        assert!(voyage.contains("provider = \"voyage\""));
        assert!(voyage.contains("model = \"voyage-4\""));

        let gemini = render_config(EmbeddingProviderChoice::Gemini);
        assert!(gemini.contains("provider = \"gemini\""));
        assert!(gemini.contains("model = \"gemini-embedding-001\""));

        let local = render_config(EmbeddingProviderChoice::Local);
        assert!(local.contains("provider = \"local\""));
        assert!(local.contains("all-MiniLM-L6-v2"));
    }

    #[test]
    fn upsert_env_var_replaces_existing_values() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env.local");
        fs::write(&path, "VOYAGE_API_KEY=old\nOTHER=keep\n").unwrap();

        upsert_env_var(&path, "VOYAGE_API_KEY", "new").unwrap();

        let updated = fs::read_to_string(path).unwrap();
        assert!(updated.contains("VOYAGE_API_KEY=new"));
        assert!(updated.contains("OTHER=keep"));
    }

    #[test]
    fn write_provider_api_key_rejects_placeholders_before_persisting() {
        let tmp = tempfile::tempdir().unwrap();

        let err = write_provider_api_key(tmp.path(), EmbeddingProviderChoice::Voyage, "changeme")
            .unwrap_err();

        assert!(err.to_string().contains("still looks like a placeholder"));
        assert!(!tmp.path().join(".env.local").exists());
    }

    #[test]
    fn append_doc_section_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("CLAUDE.md");
        fs::write(&path, "# Existing\n").unwrap();

        append_doc_section(&path, "## Tempyr Knowledge Graph\n\nBody".to_string()).unwrap();
        append_doc_section(&path, "## Tempyr Knowledge Graph\n\nBody".to_string()).unwrap();

        let updated = fs::read_to_string(path).unwrap();
        assert_eq!(updated.matches("tempyr:onboarding:start").count(), 1);
    }
}
