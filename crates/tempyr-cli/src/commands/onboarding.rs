use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::process_utils::wait_for_child_exit;

const COMMAND_HELP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingProviderChoice {
    Voyage,
    Gemini,
    Local,
}

impl EmbeddingProviderChoice {
    pub fn all() -> [Self; 3] {
        [Self::Voyage, Self::Gemini, Self::Local]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Voyage => "Voyage",
            Self::Gemini => "Google Gemini",
            Self::Local => "Local fastembed",
        }
    }

    pub fn recommendation(self) -> &'static str {
        match self {
            Self::Voyage => "Recommended premium default",
            Self::Gemini => "Recommended fallback",
            Self::Local => "Last resort / offline-friendly",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Voyage => {
                "Best default retrieval quality. Requires VOYAGE_API_KEY and uses voyage-4 / 1024 dims."
            }
            Self::Gemini => {
                "Good hosted fallback. Requires GEMINI_API_KEY and uses gemini-embedding-001 / 768 dims."
            }
            Self::Local => "No API key. Runs offline via fastembed.",
        }
    }

    pub fn env_var(self) -> Option<&'static str> {
        match self {
            Self::Voyage => Some("VOYAGE_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Local => None,
        }
    }

    pub fn needs_api_key(self) -> bool {
        self.env_var().is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingDocMode {
    Append,
    ClaudeCode,
    Codex,
    Manual,
}

impl ExistingDocMode {
    fn all() -> [Self; 4] {
        [Self::ClaudeCode, Self::Codex, Self::Append, Self::Manual]
    }

    fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Run Claude Code merge now (recommended)",
            Self::Codex => "Run Codex merge now",
            Self::Append => "Opt out: append Tempyr section directly",
            Self::Manual => "Opt out: leave unchanged, write manual steps",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::ClaudeCode => {
                "Tempyr launches Claude Code immediately to merge the Tempyr guidance into the selected docs."
            }
            Self::Codex => {
                "Tempyr launches Codex immediately to merge the Tempyr guidance into the selected docs."
            }
            Self::Append => {
                "Tempyr appends a marked Tempyr block into the selected existing markdown file without launching an agent."
            }
            Self::Manual => {
                "Tempyr leaves docs untouched and writes manual snippets under .tempyr/onboarding/."
            }
        }
    }
}

fn recommended_existing_doc_mode() -> ExistingDocMode {
    recommended_existing_doc_mode_from_availability(
        command_runs_help("claude"),
        command_runs_help("codex"),
    )
}

fn recommended_existing_doc_mode_from_availability(
    has_claude: bool,
    has_codex: bool,
) -> ExistingDocMode {
    if has_claude {
        ExistingDocMode::ClaudeCode
    } else if has_codex {
        ExistingDocMode::Codex
    } else {
        ExistingDocMode::Append
    }
}

fn command_runs_help(program: &str) -> bool {
    let mut child = match Command::new(program)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    match wait_for_child_exit(&mut child, COMMAND_HELP_TIMEOUT) {
        Ok(Some(status)) => status.success(),
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExistingDocs {
    pub claude_md: bool,
    pub agents_md: bool,
}

impl ExistingDocs {
    pub fn any(self) -> bool {
        self.claude_md || self.agents_md
    }
}

#[derive(Debug, Clone)]
pub struct OnboardingSelections {
    pub provider: EmbeddingProviderChoice,
    pub api_key: Option<String>,
    pub write_api_key_for_tempyr: bool,
    pub create_env_local_from_template: bool,
    pub validate_provider_setup: bool,
    pub run_index_rebuild: bool,
    pub install_render_overrides: bool,
    pub install_claude_hooks: bool,
    pub install_claude_skill: bool,
    pub install_claude_agent: bool,
    pub install_claude_doc: bool,
    pub install_codex_skill: bool,
    pub install_codex_doc: bool,
    pub write_mcp_setup_notes: bool,
    pub existing_doc_mode: ExistingDocMode,
}

impl OnboardingSelections {
    pub fn interactive_defaults(existing_docs: ExistingDocs) -> Self {
        Self {
            provider: EmbeddingProviderChoice::Voyage,
            api_key: None,
            write_api_key_for_tempyr: true,
            create_env_local_from_template: true,
            validate_provider_setup: true,
            run_index_rebuild: false,
            install_render_overrides: false,
            install_claude_hooks: true,
            install_claude_skill: true,
            install_claude_agent: true,
            install_claude_doc: true,
            install_codex_skill: true,
            install_codex_doc: true,
            write_mcp_setup_notes: true,
            existing_doc_mode: if existing_docs.any() {
                recommended_existing_doc_mode()
            } else {
                ExistingDocMode::Append
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Provider,
    CoreSetup,
    ApiKey,
    AgentIntegrations,
    ExistingDocs,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreOption {
    StoreApiKey,
    CreateEnvLocal,
    ValidateProvider,
    RunIndexRebuild,
    InstallRenderOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApiKeyValidation {
    Empty,
    Valid,
    Invalid(String),
}

#[derive(Debug)]
struct WizardState {
    selections: OnboardingSelections,
    existing_docs: ExistingDocs,
    page_index: usize,
    core_index: usize,
    agent_index: usize,
    existing_docs_index: usize,
    api_key_input: String,
}

impl WizardState {
    fn new(existing_docs: ExistingDocs) -> Self {
        Self {
            selections: OnboardingSelections::interactive_defaults(existing_docs),
            existing_docs,
            page_index: 0,
            core_index: 0,
            agent_index: 0,
            existing_docs_index: 0,
            api_key_input: String::new(),
        }
    }

    fn pages(&self) -> Vec<Page> {
        let mut pages = vec![Page::Welcome, Page::Provider, Page::CoreSetup];
        if self.should_show_api_key_page() {
            pages.push(Page::ApiKey);
        }
        pages.push(Page::AgentIntegrations);
        if self.selected_existing_docs() {
            pages.push(Page::ExistingDocs);
        }
        pages.push(Page::Review);
        pages
    }

    fn current_page(&self) -> Page {
        let pages = self.pages();
        pages[self.page_index.min(pages.len().saturating_sub(1))]
    }

    fn next_page(&mut self) {
        self.commit_transient_inputs();
        self.page_index = (self.page_index + 1).min(self.pages().len().saturating_sub(1));
    }

    fn prev_page(&mut self) {
        self.commit_transient_inputs();
        self.page_index = self.page_index.saturating_sub(1);
    }

    fn commit_transient_inputs(&mut self) {
        if self.should_show_api_key_page() {
            let trimmed = self.api_key_input.trim();
            self.selections.api_key = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        } else {
            self.api_key_input.clear();
            self.selections.api_key = None;
        }
    }

    fn should_show_api_key_page(&self) -> bool {
        self.selections.provider.needs_api_key() && self.selections.write_api_key_for_tempyr
    }

    fn selected_existing_docs(&self) -> bool {
        (self.existing_docs.claude_md && self.selections.install_claude_doc)
            || (self.existing_docs.agents_md && self.selections.install_codex_doc)
    }
}

pub fn run(existing_docs: ExistingDocs) -> anyhow::Result<Option<OnboardingSelections>> {
    let mut guard = TerminalGuard::enter()?;
    let mut state = WizardState::new(existing_docs);

    loop {
        guard
            .terminal
            .draw(|frame| render(frame.area(), frame, &state))
            .context("Failed to render onboarding wizard")?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if is_cancel_key(state.current_page(), key.code) {
                    return Ok(None);
                }

                match state.current_page() {
                    Page::Welcome => handle_welcome(&mut state, key.code),
                    Page::Provider => handle_provider(&mut state, key.code),
                    Page::CoreSetup => handle_core_setup(&mut state, key.code),
                    Page::ApiKey => handle_api_key(&mut state, key.code),
                    Page::AgentIntegrations => handle_agent_integrations(&mut state, key.code),
                    Page::ExistingDocs => handle_existing_docs(&mut state, key.code),
                    Page::Review => match key.code {
                        KeyCode::Enter => {
                            state.commit_transient_inputs();
                            return Ok(Some(state.selections));
                        }
                        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => {
                            state.prev_page()
                        }
                        _ => {}
                    },
                }
            }
            Event::Paste(text) if state.current_page() == Page::ApiKey => {
                append_api_key_input(&mut state, &text);
            }
            _ => {}
        }
    }
}

fn is_cancel_key(page: Page, key: KeyCode) -> bool {
    matches!(key, KeyCode::Esc) || (page != Page::ApiKey && matches!(key, KeyCode::Char('q')))
}

fn handle_welcome(state: &mut WizardState, key: KeyCode) {
    if matches!(key, KeyCode::Enter | KeyCode::Right | KeyCode::Char('n')) {
        state.next_page();
    }
}

fn handle_provider(state: &mut WizardState, key: KeyCode) {
    let current = provider_index(state.selections.provider);
    let all = EmbeddingProviderChoice::all();

    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            if current > 0 {
                update_provider(state, all[current - 1]);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if current + 1 < all.len() {
                update_provider(state, all[current + 1]);
            }
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('n') => state.next_page(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
        _ => {}
    }
}

fn handle_core_setup(state: &mut WizardState, key: KeyCode) {
    let max_index = core_options(state).len().saturating_sub(1);
    state.core_index = state.core_index.min(max_index);

    match key {
        KeyCode::Up | KeyCode::Char('k') => state.core_index = state.core_index.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            state.core_index = (state.core_index + 1).min(max_index)
        }
        KeyCode::Char(' ') => toggle_core_option(state),
        KeyCode::Enter | KeyCode::Char('n') => state.next_page(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
        KeyCode::Right | KeyCode::Char('l') => state.next_page(),
        _ => {}
    }
}

fn handle_api_key(state: &mut WizardState, key: KeyCode) {
    match key {
        KeyCode::Enter | KeyCode::Right if api_key_can_continue(state) => state.next_page(),
        KeyCode::Left => state.prev_page(),
        KeyCode::Backspace => {
            state.api_key_input.pop();
        }
        KeyCode::Delete => state.api_key_input.clear(),
        KeyCode::Char(ch) => {
            state.api_key_input.push(ch);
        }
        _ => {}
    }
}

fn handle_agent_integrations(state: &mut WizardState, key: KeyCode) {
    const MAX_INDEX: usize = 6;

    match key {
        KeyCode::Up | KeyCode::Char('k') => state.agent_index = state.agent_index.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            state.agent_index = (state.agent_index + 1).min(MAX_INDEX)
        }
        KeyCode::Char(' ') => toggle_agent_checkbox(state),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('n') => state.next_page(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
        _ => {}
    }
}

fn handle_existing_docs(state: &mut WizardState, key: KeyCode) {
    let max_index = existing_docs_row_count(state).saturating_sub(1);

    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            state.existing_docs_index = state.existing_docs_index.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.existing_docs_index = (state.existing_docs_index + 1).min(max_index);
        }
        KeyCode::Char(' ') => toggle_existing_docs_row(state),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('n') => state.next_page(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
        _ => {}
    }
}

fn provider_index(provider: EmbeddingProviderChoice) -> usize {
    EmbeddingProviderChoice::all()
        .iter()
        .position(|choice| *choice == provider)
        .unwrap_or(0)
}

fn update_provider(state: &mut WizardState, provider: EmbeddingProviderChoice) {
    let previous = state.selections.provider;
    if previous != provider {
        state.api_key_input.clear();
        state.selections.api_key = None;
    }

    state.selections.provider = provider;
    if provider.needs_api_key() {
        if !previous.needs_api_key() {
            state.selections.write_api_key_for_tempyr = true;
        }
    } else {
        state.selections.write_api_key_for_tempyr = false;
    }
    state.core_index = state
        .core_index
        .min(core_options(state).len().saturating_sub(1));
}

fn toggle_core_option(state: &mut WizardState) {
    match core_options(state).get(state.core_index).copied() {
        Some(CoreOption::StoreApiKey) => {
            state.selections.write_api_key_for_tempyr = !state.selections.write_api_key_for_tempyr;
            if !state.selections.write_api_key_for_tempyr {
                state.api_key_input.clear();
                state.selections.api_key = None;
            }
        }
        Some(CoreOption::CreateEnvLocal) => {
            state.selections.create_env_local_from_template =
                !state.selections.create_env_local_from_template;
        }
        Some(CoreOption::ValidateProvider) => {
            state.selections.validate_provider_setup = !state.selections.validate_provider_setup;
        }
        Some(CoreOption::RunIndexRebuild) => {
            state.selections.run_index_rebuild = !state.selections.run_index_rebuild;
        }
        Some(CoreOption::InstallRenderOverrides) => {
            state.selections.install_render_overrides = !state.selections.install_render_overrides;
        }
        None => {}
    }
}

fn toggle_agent_checkbox(state: &mut WizardState) {
    match state.agent_index {
        0 => state.selections.install_claude_hooks = !state.selections.install_claude_hooks,
        1 => state.selections.install_claude_skill = !state.selections.install_claude_skill,
        2 => state.selections.install_claude_agent = !state.selections.install_claude_agent,
        3 => state.selections.install_claude_doc = !state.selections.install_claude_doc,
        4 => state.selections.install_codex_skill = !state.selections.install_codex_skill,
        5 => state.selections.install_codex_doc = !state.selections.install_codex_doc,
        6 => {
            state.selections.write_mcp_setup_notes = !state.selections.write_mcp_setup_notes;
        }
        _ => {}
    }
}

fn existing_docs_row_count(state: &WizardState) -> usize {
    let mut rows = 0;
    if state.existing_docs.claude_md {
        rows += 1;
    }
    if state.existing_docs.agents_md {
        rows += 1;
    }
    rows + ExistingDocMode::all().len()
}

fn toggle_existing_docs_row(state: &mut WizardState) {
    let mut row = 0;
    if state.existing_docs.claude_md {
        if state.existing_docs_index == row {
            state.selections.install_claude_doc = !state.selections.install_claude_doc;
            return;
        }
        row += 1;
    }
    if state.existing_docs.agents_md {
        if state.existing_docs_index == row {
            state.selections.install_codex_doc = !state.selections.install_codex_doc;
            return;
        }
        row += 1;
    }

    let mode_index = state.existing_docs_index.saturating_sub(row);
    if mode_index < ExistingDocMode::all().len() {
        state.selections.existing_doc_mode = ExistingDocMode::all()[mode_index];
    }
}

fn render(area: Rect, frame: &mut ratatui::Frame<'_>, state: &WizardState) {
    let block = Block::default()
        .title(" Tempyr Onboarding ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(inner);

    let header = Paragraph::new(current_header(state)).wrap(Wrap { trim: true });
    frame.render_widget(header, chunks[0]);

    match state.current_page() {
        Page::Welcome => render_welcome(frame, chunks[1]),
        Page::Provider => render_provider(frame, chunks[1], state),
        Page::CoreSetup => render_core_setup(frame, chunks[1], state),
        Page::ApiKey => render_api_key(frame, chunks[1], state),
        Page::AgentIntegrations => render_agent_integrations(frame, chunks[1], state),
        Page::ExistingDocs => render_existing_docs(frame, chunks[1], state),
        Page::Review => render_review(frame, chunks[1], state),
    }

    let footer = Paragraph::new(current_footer(state))
        .style(Style::default().add_modifier(Modifier::DIM))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, chunks[2]);
}

fn current_header(state: &WizardState) -> Vec<Line<'static>> {
    match state.current_page() {
        Page::Welcome => vec![
            Line::from(
                "This setup walks through the project choices Tempyr needs before it writes files.",
            ),
            Line::from(
                "You will choose embeddings, optional secrets, integrations, and then confirm the plan.",
            ),
        ],
        Page::Provider => vec![
            Line::from("Choose the embedding provider for retrieval and search."),
            Line::from(
                "Hosted providers can collect an API key in-flow; local runs offline with no key.",
            ),
        ],
        Page::CoreSetup => vec![
            Line::from("Choose the setup actions Tempyr should perform during initialization."),
            Line::from(
                "These options control file scaffolding, validation, and optional post-setup work.",
            ),
        ],
        Page::ApiKey => vec![
            Line::from(format!(
                "Enter a real {} now.",
                state.selections.provider.env_var().unwrap_or("the API key")
            )),
            Line::from(
                "Tempyr validates it as you type and stores it in Tempyr's shared worktree env when available, falling back to .env.local.",
            ),
        ],
        Page::AgentIntegrations => vec![
            Line::from("Toggle the agent integrations you want Tempyr to scaffold."),
            Line::from("Hooks, skills, docs, and MCP notes can be managed independently."),
        ],
        Page::ExistingDocs => vec![
            Line::from("Existing instruction docs were found."),
            Line::from("Choose which files Tempyr should touch and how it should handle them."),
        ],
        Page::Review => vec![
            Line::from("Review the onboarding plan."),
            Line::from("Press Enter to initialize the project with these selections."),
        ],
    }
}

fn current_footer(state: &WizardState) -> &'static str {
    match state.current_page() {
        Page::Welcome => "Enter: continue  q/Esc: cancel",
        Page::Provider => "Up/Down: choose provider  Enter: continue  Backspace/Left: back",
        Page::CoreSetup => {
            "Up/Down: move  Space: toggle option  Enter: continue  Backspace/Left: back"
        }
        Page::ApiKey => {
            "Type or paste the key  Backspace: delete  Delete: clear  Enter: continue when valid  Left: back  Esc: cancel"
        }
        Page::AgentIntegrations | Page::ExistingDocs => {
            "Up/Down: move  Space: toggle/select  Enter: continue  Backspace/Left: back"
        }
        Page::Review => "Enter: confirm  Backspace/Left: back  q/Esc: cancel",
    }
}

fn render_welcome(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let text = vec![
        Line::from("This init flow will:"),
        Line::from(""),
        Line::from("1. Create .tempyr/, graph/, and the base project config."),
        Line::from("2. Ask which embedding provider this repo should use."),
        Line::from("3. Collect and validate an API key if you want Tempyr to store one."),
        Line::from("4. Scaffold Claude Code and Codex integration files."),
        Line::from("5. Review the plan before anything is written."),
    ];
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
}

fn render_provider(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(area);

    let selected = state.selections.provider;
    let rows: Vec<ListItem<'_>> = EmbeddingProviderChoice::all()
        .into_iter()
        .map(|provider| {
            ListItem::new(radio_line(
                provider == selected,
                provider == selected,
                provider.label(),
            ))
        })
        .collect();
    let list = List::new(rows)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Embedding provider "),
        );
    let mut list_state = ListState::default();
    list_state.select(Some(provider_index(selected)));
    frame.render_stateful_widget(list, columns[0], &mut list_state);

    let mut detail_lines = vec![
        Line::from(vec![
            Span::styled(
                selected.label(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::raw(format!("({})", selected.recommendation())),
        ]),
        Line::from(""),
        Line::from(selected.detail()),
        Line::from(""),
    ];
    if let Some(env_var) = selected.env_var() {
        detail_lines.push(Line::from(format!(
            "Tempyr can collect and validate {} later in setup if you keep secret storage enabled.",
            env_var
        )));
    } else {
        detail_lines.push(Line::from(
            "No secret entry screen is needed for local embeddings.",
        ));
    }

    let detail = Paragraph::new(detail_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Provider details "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, columns[1]);
}

fn render_core_setup(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(area);

    let options = core_options(state);
    let items: Vec<ListItem<'_>> = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            ListItem::new(checkbox_line(
                state.core_index == index,
                core_option_enabled(state, *option),
                &core_option_label(*option),
            ))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(Block::default().borders(Borders::ALL).title(" Core setup "));
    let mut list_state = ListState::default();
    list_state.select(Some(state.core_index.min(options.len().saturating_sub(1))));
    frame.render_stateful_widget(list, columns[0], &mut list_state);

    let detail = Paragraph::new(core_option_detail_lines(
        state,
        options
            .get(state.core_index.min(options.len().saturating_sub(1)))
            .copied()
            .unwrap_or(CoreOption::CreateEnvLocal),
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Option details "),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(detail, columns[1]);
}

fn render_api_key(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let env_var = state.selections.provider.env_var().unwrap_or("API key");
    let masked = if state.api_key_input.is_empty() {
        String::new()
    } else {
        "*".repeat(state.api_key_input.chars().count())
    };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(3),
        ])
        .split(area);

    let intro = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(env_var, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" will be written to Tempyr's shared worktree env when Git is available,"),
        ]),
        Line::from(vec![
            Span::raw("falling back to "),
            Span::styled(".env.local", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" otherwise."),
        ]),
        Line::from(""),
        Line::from("Paste the key here, or go back and disable secret storage for this provider."),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(intro, sections[0]);

    let input = Paragraph::new(masked.as_str())
        .style(Style::default().fg(Color::Yellow))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {env_var} ")),
        );
    frame.render_widget(input, sections[1]);

    let cursor_offset = masked
        .chars()
        .count()
        .min(sections[1].width.saturating_sub(2) as usize) as u16;
    frame.set_cursor_position(Position::new(
        sections[1]
            .x
            .saturating_add(1)
            .saturating_add(cursor_offset),
        sections[1].y.saturating_add(1),
    ));

    let (status_style, status_lines) = match api_key_validation(state) {
        ApiKeyValidation::Empty => (
            Style::default().fg(Color::Yellow),
            vec![
                Line::from("A real API key is required to continue from this page."),
                Line::from("Go back and turn off key storage if you want to configure it later."),
            ],
        ),
        ApiKeyValidation::Valid => (
            Style::default().fg(Color::Green),
            vec![
                Line::from("Key format looks valid."),
                Line::from("Press Enter to keep going."),
            ],
        ),
        ApiKeyValidation::Invalid(message) => {
            (Style::default().fg(Color::Red), vec![Line::from(message)])
        }
    };
    let status = Paragraph::new(status_lines)
        .style(status_style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Validation status "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(status, sections[2]);

    let help = Paragraph::new(vec![
        Line::from("The input is masked on screen and validated with Tempyr's provider checks."),
        Line::from("Delete clears the field if you want to paste a replacement."),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(help, sections[3]);
}

fn render_agent_integrations(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let rows = [
        checkbox_line(
            state.agent_index == 0,
            state.selections.install_claude_hooks,
            "Install Claude Code hooks",
        ),
        checkbox_line(
            state.agent_index == 1,
            state.selections.install_claude_skill,
            "Install Claude interview skill",
        ),
        checkbox_line(
            state.agent_index == 2,
            state.selections.install_claude_agent,
            "Install Claude extractor agent",
        ),
        checkbox_line(
            state.agent_index == 3,
            state.selections.install_claude_doc,
            "Create or update CLAUDE.md guidance",
        ),
        checkbox_line(
            state.agent_index == 4,
            state.selections.install_codex_skill,
            "Install repo-local Codex skill",
        ),
        checkbox_line(
            state.agent_index == 5,
            state.selections.install_codex_doc,
            "Create or update AGENTS.md guidance",
        ),
        checkbox_line(
            state.agent_index == 6,
            state.selections.write_mcp_setup_notes,
            "Write MCP setup notes for Claude/Codex",
        ),
    ];

    let items: Vec<ListItem<'_>> = rows.iter().map(|row| ListItem::new(row.as_str())).collect();
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Agent integrations "),
        );
    let mut list_state = ListState::default();
    list_state.select(Some(state.agent_index));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_existing_docs(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(area);

    let mut rows = Vec::new();
    let mut targets = Vec::new();

    if state.existing_docs.claude_md {
        targets.push("CLAUDE.md");
        rows.push(ListItem::new(checkbox_line(
            state.existing_docs_index == rows.len(),
            state.selections.install_claude_doc,
            "Apply existing-doc strategy to CLAUDE.md",
        )));
    }

    if state.existing_docs.agents_md {
        targets.push("AGENTS.md");
        rows.push(ListItem::new(checkbox_line(
            state.existing_docs_index == rows.len(),
            state.selections.install_codex_doc,
            "Apply existing-doc strategy to AGENTS.md",
        )));
    }

    let mode_start = rows.len();
    for mode in ExistingDocMode::all() {
        let selected = state.selections.existing_doc_mode == mode;
        rows.push(ListItem::new(format!(
            "{} ({}) {}",
            if state.existing_docs_index == rows.len() {
                ">"
            } else {
                " "
            },
            if selected { "*" } else { " " },
            mode.label()
        )));
    }

    let list = List::new(rows)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Existing docs "),
        );
    let mut list_state = ListState::default();
    list_state.select(Some(state.existing_docs_index));
    frame.render_stateful_widget(list, columns[0], &mut list_state);

    let detail_index = state.existing_docs_index.saturating_sub(mode_start);
    let detail_mode = ExistingDocMode::all()
        .get(detail_index)
        .copied()
        .unwrap_or(state.selections.existing_doc_mode);
    let detail = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Selected targets: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(
                targets
                    .into_iter()
                    .filter(|target| {
                        (*target == "CLAUDE.md" && state.selections.install_claude_doc)
                            || (*target == "AGENTS.md" && state.selections.install_codex_doc)
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ]),
        Line::from(""),
        Line::from(detail_mode.detail()),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Details "))
    .wrap(Wrap { trim: true });
    frame.render_widget(detail, columns[1]);
}

fn render_review(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Embedding provider: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.selections.provider.label()),
        ]),
        Line::from(vec![
            Span::styled(
                "Core setup: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(enabled_list(&[
                (
                    "store key",
                    state.selections.write_api_key_for_tempyr
                        && state.selections.provider.needs_api_key(),
                ),
                (
                    "env template",
                    state.selections.create_env_local_from_template,
                ),
                (
                    "validate provider",
                    state.selections.validate_provider_setup,
                ),
                ("index rebuild", state.selections.run_index_rebuild),
                (
                    "render overrides",
                    state.selections.install_render_overrides,
                ),
            ])),
        ]),
        Line::from(vec![
            Span::styled("Claude: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(enabled_list(&[
                ("hooks", state.selections.install_claude_hooks),
                ("skill", state.selections.install_claude_skill),
                ("agent", state.selections.install_claude_agent),
                ("doc", state.selections.install_claude_doc),
            ])),
        ]),
        Line::from(vec![
            Span::styled("Codex: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(enabled_list(&[
                ("skill", state.selections.install_codex_skill),
                ("doc", state.selections.install_codex_doc),
            ])),
        ]),
        Line::from(vec![
            Span::styled("MCP notes: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(if state.selections.write_mcp_setup_notes {
                "write"
            } else {
                "skip"
            }),
        ]),
    ];

    if state.selected_existing_docs() {
        lines.push(Line::from(vec![
            Span::styled(
                "Existing doc mode: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.selections.existing_doc_mode.label()),
        ]));
    }

    let widget = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Summary "))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn checkbox_line(active: bool, enabled: bool, label: &str) -> String {
    format!(
        "{} [{}] {}",
        if active { ">" } else { " " },
        if enabled { "x" } else { " " },
        label
    )
}

fn radio_line(active: bool, selected: bool, label: &str) -> String {
    format!(
        "{} ({}) {}",
        if active { ">" } else { " " },
        if selected { "x" } else { " " },
        label
    )
}

fn core_options(state: &WizardState) -> Vec<CoreOption> {
    let mut options = Vec::new();
    if state.selections.provider.needs_api_key() {
        options.push(CoreOption::StoreApiKey);
    }
    options.extend([
        CoreOption::CreateEnvLocal,
        CoreOption::ValidateProvider,
        CoreOption::RunIndexRebuild,
        CoreOption::InstallRenderOverrides,
    ]);
    options
}

fn core_option_enabled(state: &WizardState, option: CoreOption) -> bool {
    match option {
        CoreOption::StoreApiKey => state.selections.write_api_key_for_tempyr,
        CoreOption::CreateEnvLocal => state.selections.create_env_local_from_template,
        CoreOption::ValidateProvider => state.selections.validate_provider_setup,
        CoreOption::RunIndexRebuild => state.selections.run_index_rebuild,
        CoreOption::InstallRenderOverrides => state.selections.install_render_overrides,
    }
}

fn core_option_label(option: CoreOption) -> String {
    match option {
        CoreOption::StoreApiKey => "Store API key for Tempyr".to_string(),
        CoreOption::CreateEnvLocal => "Create .env.local from template if missing".to_string(),
        CoreOption::ValidateProvider => "Validate provider setup now".to_string(),
        CoreOption::RunIndexRebuild => "Run initial index rebuild after setup".to_string(),
        CoreOption::InstallRenderOverrides => {
            "Copy built-in render templates into .tempyr/render".to_string()
        }
    }
}

fn core_option_detail_lines(state: &WizardState, option: CoreOption) -> Vec<Line<'static>> {
    match option {
        CoreOption::StoreApiKey => vec![
            Line::from(vec![
                Span::styled(
                    "Store API key",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(format!(
                    "({})",
                    state
                        .selections
                        .provider
                        .env_var()
                        .unwrap_or("not required")
                )),
            ]),
            Line::from(""),
            Line::from(
                "If enabled, Tempyr opens a dedicated input screen and writes the validated key to Tempyr's shared worktree env when Git is available, falling back to .env.local.",
            ),
            Line::from(
                "Disable this if you want to keep the secret in your shell environment instead.",
            ),
        ],
        CoreOption::CreateEnvLocal => vec![
            Line::from(Span::styled(
                "Create .env.local",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Copies .env.example into .env.local when the file does not already exist."),
            Line::from("Tempyr also adds .env.local to .gitignore if needed."),
        ],
        CoreOption::ValidateProvider => vec![
            Line::from(Span::styled(
                "Validate provider setup",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(
                "Checks that the configured provider can be constructed from the current project config and environment.",
            ),
            Line::from(
                "If no hosted API key is configured yet, Tempyr reports that validation was skipped instead of failing init.",
            ),
        ],
        CoreOption::RunIndexRebuild => vec![
            Line::from(Span::styled(
                "Initial index rebuild",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Runs `tempyr index rebuild` after the project files are written."),
            Line::from("Leave this off if you want to initialize first and index later."),
        ],
        CoreOption::InstallRenderOverrides => vec![
            Line::from(Span::styled(
                "Render overrides",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(
                "Copies the built-in PRD, TDD, and task prompt templates into .tempyr/render for local customization.",
            ),
            Line::from("Leave this off if the defaults are enough for now."),
        ],
    }
}

fn api_key_validation(state: &WizardState) -> ApiKeyValidation {
    let Some(env_var) = state.selections.provider.env_var() else {
        return ApiKeyValidation::Valid;
    };
    let trimmed = state.api_key_input.trim();
    if trimmed.is_empty() {
        return ApiKeyValidation::Empty;
    }

    match tempyr_index::embeddings::validate_api_key_value(env_var, trimmed) {
        Ok(()) => ApiKeyValidation::Valid,
        Err(err) => ApiKeyValidation::Invalid(err.to_string()),
    }
}

fn api_key_can_continue(state: &WizardState) -> bool {
    matches!(api_key_validation(state), ApiKeyValidation::Valid)
}

fn append_api_key_input(state: &mut WizardState, text: &str) {
    state.api_key_input.push_str(
        &text
            .chars()
            .filter(|ch| *ch != '\r' && *ch != '\n')
            .collect::<String>(),
    );
}

fn enabled_list(entries: &[(&str, bool)]) -> String {
    let selected = entries
        .iter()
        .filter_map(|(label, enabled)| enabled.then_some(*label))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        "none".to_string()
    } else {
        selected.join(", ")
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen);
                Err(err.into())
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn cancel_shortcut_is_disabled_while_typing_api_key() {
        assert!(is_cancel_key(Page::Welcome, KeyCode::Char('q')));
        assert!(is_cancel_key(Page::ApiKey, KeyCode::Esc));
        assert!(!is_cancel_key(Page::ApiKey, KeyCode::Char('q')));
    }

    #[test]
    fn api_key_page_accepts_navigation_letters_as_input() {
        let mut state = WizardState::new(ExistingDocs {
            claude_md: false,
            agents_md: false,
        });

        handle_api_key(&mut state, KeyCode::Char('n'));
        handle_api_key(&mut state, KeyCode::Char('b'));
        handle_api_key(&mut state, KeyCode::Char('q'));

        assert_eq!(state.api_key_input, "nbq");
        assert_eq!(state.page_index, 0);
    }

    #[test]
    fn pages_include_dedicated_provider_step() {
        let state = WizardState::new(ExistingDocs {
            claude_md: false,
            agents_md: false,
        });

        assert_eq!(
            state.pages(),
            vec![
                Page::Welcome,
                Page::Provider,
                Page::CoreSetup,
                Page::ApiKey,
                Page::AgentIntegrations,
                Page::Review,
            ]
        );
    }

    #[test]
    fn existing_docs_default_prefers_live_runner() {
        assert_eq!(
            recommended_existing_doc_mode_from_availability(true, true),
            ExistingDocMode::ClaudeCode
        );
        assert_eq!(
            recommended_existing_doc_mode_from_availability(false, true),
            ExistingDocMode::Codex
        );
        assert_eq!(
            recommended_existing_doc_mode_from_availability(false, false),
            ExistingDocMode::Append
        );
        assert_eq!(ExistingDocMode::all()[0], ExistingDocMode::ClaudeCode);
    }

    #[test]
    fn command_runs_help_returns_false_for_missing_program() {
        assert!(!command_runs_help("definitely-not-a-real-tempyr-program"));
    }

    #[test]
    fn wait_for_child_exit_times_out_for_long_running_process() {
        let mut child = spawn_sleep_command();

        let status = wait_for_child_exit(&mut child, Duration::from_millis(10)).unwrap();
        assert!(status.is_none());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn api_key_page_requires_a_valid_key_before_advancing() {
        let mut state = WizardState::new(ExistingDocs {
            claude_md: false,
            agents_md: false,
        });
        state.page_index = state
            .pages()
            .iter()
            .position(|page| *page == Page::ApiKey)
            .unwrap();

        handle_api_key(&mut state, KeyCode::Enter);
        assert_eq!(state.current_page(), Page::ApiKey);

        append_api_key_input(&mut state, "changeme");
        handle_api_key(&mut state, KeyCode::Enter);
        assert_eq!(state.current_page(), Page::ApiKey);

        state.api_key_input.clear();
        append_api_key_input(&mut state, "pa-1234567890abcdef\n");
        handle_api_key(&mut state, KeyCode::Enter);

        assert_eq!(state.api_key_input, "pa-1234567890abcdef");
        assert_eq!(state.current_page(), Page::AgentIntegrations);
    }

    #[test]
    fn switching_provider_clears_staged_api_key() {
        let mut state = WizardState::new(ExistingDocs {
            claude_md: false,
            agents_md: false,
        });
        state.api_key_input = "voyage-secret".to_string();
        state.selections.api_key = Some("voyage-secret".to_string());

        update_provider(&mut state, EmbeddingProviderChoice::Gemini);

        assert_eq!(state.selections.provider, EmbeddingProviderChoice::Gemini);
        assert!(state.api_key_input.is_empty());
        assert!(state.selections.api_key.is_none());
    }

    #[cfg(windows)]
    fn spawn_sleep_command() -> std::process::Child {
        Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "Start-Sleep -Seconds 5",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(not(windows))]
    fn spawn_sleep_command() -> std::process::Child {
        Command::new("sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}
