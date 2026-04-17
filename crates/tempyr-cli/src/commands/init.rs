use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::config::ProjectContext;
use anyhow::Context;

use super::git_hooks;
use super::index_cmd;
use super::managed::{self, ManagedArtifact, WriteOutcome};
use super::onboarding::{
    self, EmbeddingProviderChoice, ExistingDocMode, ExistingDocs, OnboardingSelections,
};
use super::process_utils::wait_for_child_exit;
use tempyr_core::project;
use tempyr_index::embeddings;

const DEFAULT_SCHEMA: &str = include_str!("../../../../schema/default-schema.toml");
const CLAUDE_DOC_TEMPLATE: &str = include_str!("../../assets/CLAUDE.template.md");
const AGENTS_DOC_TEMPLATE: &str = include_str!("../../assets/AGENTS.template.md");
const CODEX_CONFIG_TEMPLATE: &str = include_str!("../../assets/codex.config.toml");
const CODEX_SKILL_TEMPLATE: &str = include_str!("../../assets/tempyr-interview.codex.SKILL.md");
const PRD_TEMPLATE: &str = include_str!("../../../../templates/prd.toml");
const TDD_TEMPLATE: &str = include_str!("../../../../templates/tdd.toml");
const TASK_PROMPT_TEMPLATE: &str = include_str!("../../../../templates/task-prompt.toml");
const MERGE_AGENT_TIMEOUT: Duration = Duration::from_secs(30);

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
        write_api_key_for_tempyr: false,
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

    if selections.write_api_key_for_tempyr
        && let Some(key) = selections.api_key.as_deref()
    {
        let (env_var, path) = write_provider_api_key(root, selections.provider, key)?;
        summary.push(format!(
            "  {:<20} - stored {}",
            display_api_key_target(root, &path),
            env_var
        ));
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

    if should_write_codex_config(selections, &existing_doc_updates) {
        let outcome = write_codex_config(root)?;
        summary.push(format!(
            "  .codex/config.toml  - {}",
            outcome.label("repo-local Codex sandbox config")
        ));
    }

    if !existing_doc_updates.is_empty() {
        match selections.existing_doc_mode {
            ExistingDocMode::ClaudeCode => {
                summary.extend(run_existing_doc_agent_merge(
                    root,
                    &existing_doc_updates,
                    MergeAgent::ClaudeCode,
                )?);
            }
            ExistingDocMode::Codex => {
                summary.extend(run_existing_doc_agent_merge(
                    root,
                    &existing_doc_updates,
                    MergeAgent::Codex,
                )?);
            }
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
            "# API key: set VOYAGE_API_KEY in Tempyr's shared worktree env, .env.local, or your shell environment",
        ),
        EmbeddingProviderChoice::Gemini => (
            "gemini",
            "gemini-embedding-001",
            768,
            "# API key: set GEMINI_API_KEY in Tempyr's shared worktree env, .env.local, or your shell environment",
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
) -> anyhow::Result<(&'static str, PathBuf)> {
    let env_var = provider
        .env_var()
        .ok_or_else(|| anyhow::anyhow!("Selected provider does not use an API key"))?;
    embeddings::validate_api_key_value(env_var, key)?;
    let path = provider_api_key_path(root);
    upsert_env_var(&path, env_var, key)?;
    if path == root.join(".env.local") {
        ensure_gitignore_contains(root, ".env.local")?;
    }
    Ok((env_var, path))
}

fn provider_api_key_path(root: &Path) -> PathBuf {
    project::shared_env_root(root)
        .unwrap_or_else(|| root.to_path_buf())
        .join(".env.local")
}

fn display_api_key_target(root: &Path, path: &Path) -> String {
    if path == root.join(".env.local") {
        ".env.local".to_string()
    } else if let Some(shared_root) = project::shared_env_root(root)
        && path == shared_root.join(".env.local")
    {
        "<git-common-dir>/tempyr/.env.local".to_string()
    } else {
        path.display().to_string()
    }
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

- Prefer a project-level `.mcp.json` in the repo root so the MCP config is shared and follows each Git worktree.
- Use relative paths in project `.mcp.json` entries. Anthropic documents relative paths for project-scoped `.mcp.json` and absolute paths for user-level `~/.claude.json`.
- For hosted embedding keys shared across worktrees, prefer Tempyr's shared Git-common-dir env file at `<git-common-dir>/tempyr/.env.local`. Tempyr loads that automatically without committing it.
- If `tempyr` is already on `PATH`, use a minimal project config like:

```json
{
  "mcpServers": {
    "tempyr": {
      "command": "tempyr",
      "args": ["--mcp"],
      "env": {}
    }
  }
}
```

- If `tempyr` is not reliably on `PATH`, or you want deterministic worktree-local project discovery regardless of subprocess launch details, point `.mcp.json` at a repo-relative launcher script that sets `TEMPYR_PROJECT_ROOT` from its own location before execing `tempyr --mcp`.
- If Tempyr needs repo-local `.env` or `.env.local` files, add them to `.worktreeinclude` so Claude-created worktrees copy those gitignored files.
- Keep Claude approval choices and other machine-specific trust settings local instead of checking them into the repo.
- If you want Claude to merge existing instruction docs, prefer `--permission-mode acceptEdits` with narrow `Edit(...)` tool rules for the target markdown files.

## Codex

- Prefer a project-scoped `.codex/config.toml` entry instead of a user-level `~/.codex/config.toml` entry when you want MCP to follow Git worktrees.
- In that project config, set `cwd = ".."` so the MCP server starts from the repo root even though `.codex/config.toml` lives under `.codex/`.
- For hosted embedding keys shared across worktrees, prefer Tempyr's shared Git-common-dir env file at `<git-common-dir>/tempyr/.env.local`. Tempyr loads that automatically without committing it.
- Example:

```toml
[mcp_servers.tempyr]
command = "tempyr"
args = ["--mcp"]
cwd = ".."
startup_timeout_sec = 5
```

- Avoid an absolute `cwd` in shared or user-level config if you want the same checked-in config to work across multiple worktrees.
- Use `TEMPYR_PROJECT_ROOT` (or `TEMPYR_GRAPH_DIR`) only as a fallback escape hatch when the MCP client cannot launch the server from the correct working directory.
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
    if let Some(env_var) = embeddings::provider_api_key_env_var(&resolved.provider) {
        let value = match std::env::var(env_var) {
            Ok(value) => value,
            Err(_) => return Ok(format!("skipped ({env_var} not set yet)")),
        };
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

fn write_codex_config(root: &Path) -> anyhow::Result<SupportWriteOutcome> {
    write_support_file(&codex_config_path(root), CODEX_CONFIG_TEMPLATE)
}

fn codex_config_path(root: &Path) -> PathBuf {
    root.join(".codex").join("config.toml")
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
            body.push_str("- Keep Tempyr in a project-level `.mcp.json` at the repo root so the MCP config is shared and follows Git worktrees.\n");
            body.push_str("- Prefer relative paths in that `.mcp.json`; keep user-level `~/.claude.json` entries for personal servers, not worktree-local Tempyr config.\n");
            body.push_str("- For hosted embedding keys shared across worktrees, prefer Tempyr's shared Git-common-dir env file at `<git-common-dir>/tempyr/.env.local`; Tempyr loads it automatically.\n");
            body.push_str("- If `tempyr` is not reliably on `PATH`, use a repo-relative launcher script that derives `TEMPYR_PROJECT_ROOT` from its own location before execing `tempyr --mcp`.\n");
            body.push_str("- Add `.env` and `.env.local` to `.worktreeinclude` when Tempyr needs provider credentials inside Claude-created worktrees.\n");
            body.push_str("- Use `--permission-mode acceptEdits`.\n");
            body.push_str("- Prefer `--allowedTools Read,Grep,Glob,Edit(/CLAUDE.md),Edit(/AGENTS.md)` to narrow writes to the instruction docs you want merged.\n");
            body.push_str("- Keep approval choices local instead of checking shared allowlists into the repo.\n");
            body.push_str("- Supporting `.claude` hooks/skills were installed directly by Tempyr because protected directories can still prompt.\n\n");
        }
        FollowUpMode::Codex => {
            body.push_str("Suggested Codex setup notes:\n");
            body.push_str("- Use project-scoped `.codex/config.toml` with `sandbox_mode`, `approval_policy`, and `sandbox_workspace_write.writable_roots` tuned to the doc files you want updated.\n");
            body.push_str("- For Tempyr MCP in that project config, set `cwd = \"..\"` so Codex launches `tempyr --mcp` from the repo root while keeping the config worktree-portable.\n");
            body.push_str("- For hosted embedding keys shared across worktrees, prefer Tempyr's shared Git-common-dir env file at `<git-common-dir>/tempyr/.env.local`; Tempyr loads it automatically.\n");
            body.push_str("- Avoid an absolute MCP `cwd` in shared or user-level config if you want the same setup to follow Git worktrees cleanly.\n");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeAgent {
    ClaudeCode,
    Codex,
}

impl MergeAgent {
    fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    fn fallback_mode(self) -> FollowUpMode {
        match self {
            Self::ClaudeCode => FollowUpMode::ClaudeCode,
            Self::Codex => FollowUpMode::Codex,
        }
    }
}

fn run_existing_doc_agent_merge(
    root: &Path,
    docs: &[DocSpec],
    agent: MergeAgent,
) -> anyhow::Result<Vec<String>> {
    let before = doc_snapshots(root, docs)?;
    let prompt = render_agent_merge_prompt(docs);

    let output = match run_merge_agent_command(root, docs, agent, &prompt) {
        Ok(output) => output,
        Err(err) => {
            return restore_and_fallback_agent_merge(
                root,
                docs,
                agent,
                &before,
                format!(
                    "Failed to restore original docs after {} merge failure",
                    agent.label()
                ),
                err.to_string(),
            );
        }
    };

    if !output.status.success() {
        let detail = summarize_process_output(&output.stdout, &output.stderr);
        return restore_and_fallback_agent_merge(
            root,
            docs,
            agent,
            &before,
            format!(
                "Failed to restore original docs after {} exited unsuccessfully",
                agent.label()
            ),
            format!("{} exited with {}", agent.label(), output.status),
        )
        .map(|mut lines| {
            if !detail.is_empty() {
                lines.push(format!("  existing docs       - {}", detail));
            }
            lines
        });
    }

    let after = match doc_snapshots(root, docs) {
        Ok(after) => after,
        Err(err) => {
            return restore_and_fallback_agent_merge(
                root,
                docs,
                agent,
                &before,
                format!(
                    "Failed to restore original docs after {} produced unreadable merged docs",
                    agent.label()
                ),
                format!(
                    "{} completed but Tempyr could not read the merged docs: {}",
                    agent.label(),
                    err
                ),
            );
        }
    };
    let mut lines = Vec::new();
    if let Some(warning) = codex_missing_config_warning(root, agent) {
        lines.push(warning);
    }
    lines.push(format!(
        "  existing docs       - launched {} to merge instruction docs",
        agent.label()
    ));

    let mut changed = 0usize;
    for (path, before_contents) in before {
        let after_contents = after.get(&path).map(String::as_str).unwrap_or_default();
        if before_contents != after_contents {
            changed += 1;
            lines.push(format!(
                "  {}               - merged by {}",
                path,
                agent.label()
            ));
        }
    }

    if changed == 0 {
        lines.push(format!(
            "  existing docs       - {} completed but no doc changes were detected",
            agent.label()
        ));
    }

    Ok(lines)
}

fn restore_and_fallback_agent_merge(
    root: &Path,
    docs: &[DocSpec],
    agent: MergeAgent,
    before: &std::collections::BTreeMap<&'static str, String>,
    restore_context: String,
    reason: String,
) -> anyhow::Result<Vec<String>> {
    restore_doc_snapshots(root, before).with_context(|| restore_context)?;
    fallback_agent_merge_summary(root, docs, agent, reason)
}

fn fallback_agent_merge_summary(
    root: &Path,
    docs: &[DocSpec],
    agent: MergeAgent,
    reason: String,
) -> anyhow::Result<Vec<String>> {
    let path = write_doc_follow_up(root, docs, agent.fallback_mode())?;
    let mut lines = vec![format!(
        "  existing docs       - warning: could not run {} merge automatically: {}",
        agent.label(),
        reason
    )];
    if let Some(warning) = codex_missing_config_warning(root, agent) {
        lines.push(warning);
    }
    lines.push(format!(
        "  {}  - {} handoff prompt",
        display_relative(root, &path),
        agent.label()
    ));
    Ok(lines)
}

fn run_merge_agent_command(
    root: &Path,
    docs: &[DocSpec],
    agent: MergeAgent,
    prompt: &str,
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(agent.command());
    command.current_dir(root);
    command.args(merge_agent_command_args(docs, agent, prompt));
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to launch {} for existing-doc merge", agent.label()))?;
    let stdout_reader = spawn_output_reader(child.stdout.take().with_context(|| {
        format!(
            "Failed to capture {} existing-doc merge stdout",
            agent.label()
        )
    })?);
    let stderr_reader = spawn_output_reader(child.stderr.take().with_context(|| {
        format!(
            "Failed to capture {} existing-doc merge stderr",
            agent.label()
        )
    })?);
    let completed = wait_for_child_exit(&mut child, MERGE_AGENT_TIMEOUT).with_context(|| {
        format!(
            "Failed while waiting for {} existing-doc merge to finish",
            agent.label()
        )
    })?;

    if completed.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        anyhow::bail!(
            "Timed out waiting for {} existing-doc merge after {} seconds",
            agent.label(),
            MERGE_AGENT_TIMEOUT.as_secs()
        );
    }

    Ok(std::process::Output {
        status: completed.expect("status checked above"),
        stdout: join_output_reader(stdout_reader, agent, "stdout")?,
        stderr: join_output_reader(stderr_reader, agent, "stderr")?,
    })
}

fn spawn_output_reader<T>(mut reader: T) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    T: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_output_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    agent: MergeAgent,
    stream_name: &str,
) -> anyhow::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| {
            anyhow::anyhow!(
                "{} existing-doc merge {} reader thread panicked",
                agent.label(),
                stream_name
            )
        })?
        .with_context(|| {
            format!(
                "Failed to read {} existing-doc merge {}",
                agent.label(),
                stream_name
            )
        })
}

fn merge_agent_command_args(docs: &[DocSpec], agent: MergeAgent, prompt: &str) -> Vec<String> {
    match agent {
        MergeAgent::ClaudeCode => vec![
            "--print".to_string(),
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
            "--allowedTools".to_string(),
            claude_allowed_tools(docs),
            "--output-format".to_string(),
            "text".to_string(),
            prompt.to_string(),
        ],
        MergeAgent::Codex => vec!["--auto-edit".to_string(), prompt.to_string()],
    }
}

fn claude_allowed_tools(docs: &[DocSpec]) -> String {
    let mut tools = vec!["Read".to_string(), "Grep".to_string(), "Glob".to_string()];
    for doc in docs {
        tools.push(format!("Edit(/{})", doc.path));
    }
    tools.join(",")
}

fn codex_missing_config_warning(root: &Path, agent: MergeAgent) -> Option<String> {
    if agent == MergeAgent::Codex && !codex_config_path(root).is_file() {
        Some(
            "  existing docs       - warning: Codex is running without a repo-local .codex/config.toml; configure sandboxed writable roots before relying on Codex merges".to_string(),
        )
    } else {
        None
    }
}

fn render_agent_merge_prompt(docs: &[DocSpec]) -> String {
    let mut body = String::from(
        "Merge the Tempyr guidance into the existing instruction docs in this repository.\n\n",
    );
    body.push_str("Rules:\n");
    body.push_str("- Edit only the files listed below.\n");
    body.push_str("- Preserve project-specific instructions and formatting.\n");
    body.push_str("- Merge the Tempyr guidance where it fits naturally instead of blindly appending duplicates.\n");
    body.push_str("- If a Tempyr section already exists, update it in place so each file ends up with one coherent Tempyr section.\n");
    body.push_str("- Do not create prompt files, notes, or any non-target files.\n");
    body.push_str("- When finished, briefly report which files changed.\n\n");

    for doc in docs {
        body.push_str(&format!("Target file: {}\n\n", doc.path));
        body.push_str("Required Tempyr guidance:\n");
        body.push_str("```md\n");
        body.push_str(&doc.section());
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n\n");
    }

    body
}

fn doc_snapshots(
    root: &Path,
    docs: &[DocSpec],
) -> anyhow::Result<std::collections::BTreeMap<&'static str, String>> {
    let mut snapshots = std::collections::BTreeMap::new();
    for doc in docs {
        snapshots.insert(doc.path, fs::read_to_string(root.join(doc.path))?);
    }
    Ok(snapshots)
}

fn restore_doc_snapshots(
    root: &Path,
    snapshots: &std::collections::BTreeMap<&'static str, String>,
) -> anyhow::Result<()> {
    for (path, contents) in snapshots {
        fs::write(root.join(path), contents)?;
    }
    Ok(())
}

fn should_write_codex_config(
    selections: &OnboardingSelections,
    existing_doc_updates: &[DocSpec],
) -> bool {
    selections.existing_doc_mode == ExistingDocMode::Codex && !existing_doc_updates.is_empty()
}

fn summarize_process_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr.is_empty() {
        return truncate_summary_line(&stderr);
    }

    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    truncate_summary_line(&stdout)
}

fn truncate_summary_line(text: &str) -> String {
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.chars().count() <= 160 {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed.chars().take(157).collect::<String>())
    }
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
    use std::ffi::OsString;
    use std::sync::{LazyLock, Mutex, MutexGuard};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(String, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new(vars: &[&str]) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let previous = vars
                .iter()
                .map(|var| ((*var).to_string(), std::env::var_os(var)))
                .collect();
            Self {
                _lock: lock,
                previous,
            }
        }

        fn clear(&self, var: &str) {
            unsafe {
                std::env::remove_var(var);
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, previous) in self.previous.drain(..) {
                unsafe {
                    match previous {
                        Some(value) => std::env::set_var(&var, value),
                        None => std::env::remove_var(&var),
                    }
                }
            }
        }
    }

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
    fn validate_provider_setup_skips_missing_hosted_key() {
        let env = EnvGuard::new(&["VOYAGE_API_KEY"]);
        env.clear("VOYAGE_API_KEY");

        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".tempyr")).unwrap();
        fs::create_dir_all(tmp.path().join("graph")).unwrap();
        fs::write(tmp.path().join(".tempyr/schema.toml"), DEFAULT_SCHEMA).unwrap();
        fs::write(
            tmp.path().join(".tempyr/config.toml"),
            render_config(EmbeddingProviderChoice::Voyage),
        )
        .unwrap();

        let message = validate_provider_setup(tmp.path()).unwrap();

        assert_eq!(message, "skipped (VOYAGE_API_KEY not set yet)");
    }

    #[test]
    fn claude_merge_uses_accept_edits_and_restricted_tools() {
        let docs = [
            DocSpec::new("CLAUDE.md", "# Claude\n", "Claude Code instructions"),
            DocSpec::new("AGENTS.md", "# Agents\n", "Codex / agent instructions"),
        ];

        let args = merge_agent_command_args(&docs, MergeAgent::ClaudeCode, "merge prompt");

        assert_eq!(
            args,
            vec![
                "--print",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Read,Grep,Glob,Edit(/CLAUDE.md),Edit(/AGENTS.md)",
                "--output-format",
                "text",
                "merge prompt",
            ]
        );
    }

    #[test]
    fn codex_merge_uses_auto_edit_mode() {
        let args = merge_agent_command_args(&[], MergeAgent::Codex, "merge prompt");

        assert_eq!(args, vec!["--auto-edit", "merge prompt"]);
    }

    #[test]
    fn should_write_codex_config_for_codex_merge_mode() {
        let selections = OnboardingSelections {
            install_codex_skill: false,
            install_codex_doc: false,
            existing_doc_mode: ExistingDocMode::Codex,
            ..OnboardingSelections::interactive_defaults(ExistingDocs {
                claude_md: true,
                agents_md: false,
            })
        };
        let docs = [DocSpec::new(
            "CLAUDE.md",
            "# Claude\n",
            "Claude Code instructions",
        )];

        assert!(should_write_codex_config(&selections, &docs));
    }

    #[test]
    fn should_not_write_codex_config_for_plain_codex_installs() {
        let selections = OnboardingSelections {
            install_codex_skill: true,
            install_codex_doc: true,
            existing_doc_mode: ExistingDocMode::Append,
            ..OnboardingSelections::interactive_defaults(ExistingDocs {
                claude_md: false,
                agents_md: false,
            })
        };
        let docs = [DocSpec::new(
            "AGENTS.md",
            "# Agents\n",
            "Codex / agent instructions",
        )];

        assert!(!should_write_codex_config(&selections, &docs));
    }

    #[test]
    fn restore_doc_snapshots_rewrites_original_contents() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "changed").unwrap();

        let mut snapshots = std::collections::BTreeMap::new();
        snapshots.insert("CLAUDE.md", "original".to_string());

        restore_doc_snapshots(tmp.path(), &snapshots).unwrap();

        assert_eq!(
            fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap(),
            "original"
        );
    }

    #[test]
    fn fallback_summary_warns_when_codex_config_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = [DocSpec::new(
            "AGENTS.md",
            "# Agents\n",
            "Codex / agent instructions",
        )];

        let lines = fallback_agent_merge_summary(
            tmp.path(),
            &docs,
            MergeAgent::Codex,
            "Codex exited with status 1".to_string(),
        )
        .unwrap();

        assert!(
            lines
                .iter()
                .any(|line| line
                    .contains("Codex is running without a repo-local .codex/config.toml"))
        );
    }

    #[test]
    fn truncate_summary_line_handles_unicode_safely() {
        let text = format!("{}\nsecond line", "é".repeat(170));

        let truncated = truncate_summary_line(&text);

        assert_eq!(truncated, format!("{}...", "é".repeat(157)));
    }

    #[test]
    fn summarize_process_output_prefers_stderr() {
        let detail = summarize_process_output(b"stdout detail", "stderr detail".as_bytes());

        assert_eq!(detail, "stderr detail");
    }

    #[test]
    fn write_provider_api_key_prefers_shared_git_env_when_available() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();

        let (env_var, path) =
            write_provider_api_key(tmp.path(), EmbeddingProviderChoice::Voyage, "pa-valid-key")
                .unwrap();

        assert_eq!(env_var, "VOYAGE_API_KEY");
        assert_eq!(
            path,
            fs::canonicalize(tmp.path().join(".git").join("tempyr").join(".env.local")).unwrap()
        );
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("VOYAGE_API_KEY=pa-valid-key")
        );
        assert!(!tmp.path().join(".env.local").exists());
    }

    #[test]
    fn write_provider_api_key_uses_git_common_dir_for_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("wt");
        let common = repo.join(".git");
        let private = common.join("worktrees").join("feature");

        fs::create_dir_all(&private).unwrap();
        fs::create_dir(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .unwrap();
        fs::write(private.join("commondir"), "../..\n").unwrap();

        let (env_var, path) =
            write_provider_api_key(&worktree, EmbeddingProviderChoice::Voyage, "pa-valid-key")
                .unwrap();

        assert_eq!(env_var, "VOYAGE_API_KEY");
        assert_eq!(
            path,
            fs::canonicalize(common.join("tempyr").join(".env.local")).unwrap()
        );
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("VOYAGE_API_KEY=pa-valid-key")
        );
        assert!(!worktree.join(".env.local").exists());
        assert!(!private.join("tempyr").join(".env.local").exists());
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
