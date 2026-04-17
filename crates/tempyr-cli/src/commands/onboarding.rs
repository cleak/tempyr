use std::io;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

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
            Self::Local => {
                "No API key, but requires tempyr to be built with --features local-embeddings."
            }
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
            Self::Append => "Append Tempyr section",
            Self::ClaudeCode => "Prepare Claude Code handoff",
            Self::Codex => "Prepare Codex handoff",
            Self::Manual => "Leave unchanged, write manual steps",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Append => {
                "Tempyr appends a marked Tempyr block into the selected existing markdown file."
            }
            Self::ClaudeCode => {
                "Tempyr leaves existing docs untouched and writes a Claude Code merge prompt."
            }
            Self::Codex => "Tempyr leaves existing docs untouched and writes a Codex merge prompt.",
            Self::Manual => {
                "Tempyr leaves docs untouched and writes manual snippets under .tempyr/onboarding/."
            }
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
        let existing_doc_mode = if existing_docs.any() {
            ExistingDocMode::ClaudeCode
        } else {
            ExistingDocMode::Append
        };

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
            existing_doc_mode,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    CoreSetup,
    ApiKey,
    AgentIntegrations,
    ExistingDocs,
    Review,
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
        let mut pages = vec![Page::Welcome, Page::CoreSetup];
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

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if is_cancel_key(state.current_page(), key.code) {
            return Ok(None);
        }

        match state.current_page() {
            Page::Welcome => handle_welcome(&mut state, key.code),
            Page::CoreSetup => handle_core_setup(&mut state, key.code),
            Page::ApiKey => handle_api_key(&mut state, key.code),
            Page::AgentIntegrations => handle_agent_integrations(&mut state, key.code),
            Page::ExistingDocs => handle_existing_docs(&mut state, key.code),
            Page::Review => match key.code {
                KeyCode::Enter => {
                    state.commit_transient_inputs();
                    return Ok(Some(state.selections));
                }
                KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
                _ => {}
            },
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

fn handle_core_setup(state: &mut WizardState, key: KeyCode) {
    const MAX_INDEX: usize = 5;

    match key {
        KeyCode::Up | KeyCode::Char('k') => state.core_index = state.core_index.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            state.core_index = (state.core_index + 1).min(MAX_INDEX)
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if state.core_index == 0 {
                let current = provider_index(state.selections.provider);
                if current > 0 {
                    update_provider(state, EmbeddingProviderChoice::all()[current - 1]);
                }
            } else {
                state.prev_page();
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if state.core_index == 0 {
                let current = provider_index(state.selections.provider);
                let all = EmbeddingProviderChoice::all();
                if current + 1 < all.len() {
                    update_provider(state, all[current + 1]);
                }
            } else {
                state.next_page();
            }
        }
        KeyCode::Char(' ') => toggle_core_checkbox(state),
        KeyCode::Enter | KeyCode::Char('n') => state.next_page(),
        KeyCode::Backspace | KeyCode::Char('b') => state.prev_page(),
        _ => {}
    }
}

fn handle_api_key(state: &mut WizardState, key: KeyCode) {
    match key {
        KeyCode::Enter | KeyCode::Right => state.next_page(),
        KeyCode::Left => state.prev_page(),
        KeyCode::Backspace => {
            state.api_key_input.pop();
        }
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
    if state.selections.provider != provider {
        state.api_key_input.clear();
        state.selections.api_key = None;
    }

    state.selections.provider = provider;
    if !provider.needs_api_key() {
        state.selections.write_api_key_for_tempyr = false;
    }
}

fn toggle_core_checkbox(state: &mut WizardState) {
    match state.core_index {
        1 => {
            if state.selections.provider.needs_api_key() {
                state.selections.write_api_key_for_tempyr =
                    !state.selections.write_api_key_for_tempyr;
                if !state.selections.write_api_key_for_tempyr {
                    state.api_key_input.clear();
                }
            }
        }
        2 => {
            state.selections.create_env_local_from_template =
                !state.selections.create_env_local_from_template;
        }
        3 => {
            state.selections.validate_provider_setup = !state.selections.validate_provider_setup;
        }
        4 => {
            state.selections.run_index_rebuild = !state.selections.run_index_rebuild;
        }
        5 => {
            state.selections.install_render_overrides = !state.selections.install_render_overrides;
        }
        _ => {}
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
            Line::from("Set up Tempyr with grouped onboarding pages and opinionated defaults."),
            Line::from("Most pages combine several setup decisions so the flow stays short."),
        ],
        Page::CoreSetup => vec![
            Line::from("Choose the embedding provider, then toggle the adjacent setup actions."),
            Line::from("Voyage is the premium default, then Gemini, then local as the fallback."),
        ],
        Page::ApiKey => vec![
            Line::from(format!(
                "Enter {} now or leave it blank to fill in later.",
                state.selections.provider.env_var().unwrap_or("the API key")
            )),
            Line::from(
                "Tempyr will only write the value if 'Store API key for Tempyr' stays enabled.",
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
        Page::CoreSetup => {
            "Up/Down: move  Left/Right: change provider  Space: toggle option  Enter: continue"
        }
        Page::ApiKey => {
            "Type to enter the key  Backspace: delete  Enter/Right: continue  Left: back  Esc: cancel"
        }
        Page::AgentIntegrations | Page::ExistingDocs => {
            "Up/Down: move  Space: toggle/select  Enter: continue  Backspace/Left: back"
        }
        Page::Review => "Enter: confirm  Backspace/Left: back  q/Esc: cancel",
    }
}

fn render_welcome(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let text = vec![
        Line::from("Tempyr will:"),
        Line::from(""),
        Line::from("- create .tempyr/ and graph/"),
        Line::from("- configure embeddings and optional secrets"),
        Line::from("- scaffold Claude Code and Codex integrations"),
        Line::from("- write follow-up notes instead of clobbering existing docs"),
    ];
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
}

fn render_core_setup(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(area);

    let provider = state.selections.provider;
    let rows = [
        format!(
            "{} Embedding provider: {} ({})",
            if state.core_index == 0 { ">" } else { " " },
            provider.label(),
            provider.recommendation()
        ),
        checkbox_line(
            state.core_index == 1,
            state.selections.write_api_key_for_tempyr && provider.needs_api_key(),
            if provider.needs_api_key() {
                "Store API key for Tempyr"
            } else {
                "Store API key for Tempyr (not needed for local)"
            },
        ),
        checkbox_line(
            state.core_index == 2,
            state.selections.create_env_local_from_template,
            "Create .env.local from template if missing",
        ),
        checkbox_line(
            state.core_index == 3,
            state.selections.validate_provider_setup,
            "Validate provider setup now",
        ),
        checkbox_line(
            state.core_index == 4,
            state.selections.run_index_rebuild,
            "Run initial index rebuild after setup",
        ),
        checkbox_line(
            state.core_index == 5,
            state.selections.install_render_overrides,
            "Copy built-in render templates into .tempyr/render",
        ),
    ];

    let items: Vec<ListItem<'_>> = rows.iter().map(|row| ListItem::new(row.as_str())).collect();
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(Block::default().borders(Borders::ALL).title(" Core setup "));
    let mut list_state = ListState::default();
    list_state.select(Some(state.core_index));
    frame.render_stateful_widget(list, columns[0], &mut list_state);

    let detail = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                provider.label(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::raw(format!("({})", provider.recommendation())),
        ]),
        Line::from(""),
        Line::from(provider.detail()),
        Line::from(""),
        Line::from("Local note: local embeddings only work when this binary was built with"),
        Line::from("`--features local-embeddings`."),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Provider details "),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(detail, columns[1]);
}

fn render_api_key(frame: &mut ratatui::Frame<'_>, area: Rect, state: &WizardState) {
    let env_var = state.selections.provider.env_var().unwrap_or("API key");
    let masked = if state.api_key_input.is_empty() {
        "<leave blank to configure later>".to_string()
    } else {
        "*".repeat(state.api_key_input.chars().count())
    };

    let text = vec![
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
        Line::from(vec![
            Span::styled(
                "Current input: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(masked),
        ]),
        Line::from(""),
        Line::from(
            "Leave this empty if you want Tempyr to scaffold config without storing the secret yet.",
        ),
    ];
    let widget = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" API key "))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
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
        if let Err(err) = execute!(stdout, EnterAlternateScreen) {
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
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
