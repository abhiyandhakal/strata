use std::fmt;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tui_textarea::{CursorMove, Input, Key, Scrolling, TextArea};

use crate::core::{Cell, CellKind, ExecutionStatus, Language, Notebook};
use crate::runtime::SessionManager;
use crate::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppMode {
    Normal,
    Edit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VimMode {
    Normal,
    Insert,
    Visual,
    Operator(char),
}

impl fmt::Display for VimMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VimMode::Normal => write!(f, "NORMAL"),
            VimMode::Insert => write!(f, "INSERT"),
            VimMode::Visual => write!(f, "VISUAL"),
            VimMode::Operator(op) => write!(f, "OPERATOR({op})"),
        }
    }
}

enum VimTransition {
    Nop,
    Mode(VimMode),
    Pending(Input),
}

#[derive(Clone, Debug)]
struct VimState {
    mode: VimMode,
    pending: Input,
}

impl VimState {
    fn new(mode: VimMode) -> Self {
        Self {
            mode,
            pending: Input::default(),
        }
    }

    fn with_pending(self, pending: Input) -> Self {
        Self {
            mode: self.mode,
            pending,
        }
    }

    fn transition(&self, input: Input, textarea: &mut TextArea<'_>) -> VimTransition {
        if input.key == Key::Null {
            return VimTransition::Nop;
        }

        match self.mode {
            VimMode::Normal | VimMode::Visual | VimMode::Operator(_) => {
                match input {
                    Input {
                        key: Key::Char('h'),
                        ..
                    } => textarea.move_cursor(CursorMove::Back),
                    Input {
                        key: Key::Char('j'),
                        ..
                    } => textarea.move_cursor(CursorMove::Down),
                    Input {
                        key: Key::Char('k'),
                        ..
                    } => textarea.move_cursor(CursorMove::Up),
                    Input {
                        key: Key::Char('l'),
                        ..
                    } => textarea.move_cursor(CursorMove::Forward),
                    Input {
                        key: Key::Char('w'),
                        ..
                    } => textarea.move_cursor(CursorMove::WordForward),
                    Input {
                        key: Key::Char('e'),
                        ctrl: false,
                        ..
                    } => {
                        textarea.move_cursor(CursorMove::WordEnd);
                        if matches!(self.mode, VimMode::Operator(_)) {
                            textarea.move_cursor(CursorMove::Forward);
                        }
                    }
                    Input {
                        key: Key::Char('b'),
                        ctrl: false,
                        ..
                    } => textarea.move_cursor(CursorMove::WordBack),
                    Input {
                        key: Key::Char('^'),
                        ..
                    }
                    | Input {
                        key: Key::Char('0'),
                        ..
                    } => textarea.move_cursor(CursorMove::Head),
                    Input {
                        key: Key::Char('$'),
                        ..
                    } => textarea.move_cursor(CursorMove::End),
                    Input {
                        key: Key::Char('D'),
                        ..
                    } => {
                        textarea.delete_line_by_end();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('C'),
                        ..
                    } => {
                        textarea.delete_line_by_end();
                        textarea.cancel_selection();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('p'),
                        ..
                    } => {
                        textarea.paste();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('u'),
                        ctrl: false,
                        ..
                    } => {
                        textarea.undo();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('r'),
                        ctrl: true,
                        ..
                    } => {
                        textarea.redo();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('x'),
                        ..
                    } => {
                        textarea.delete_next_char();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('s'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Normal => {
                        textarea.cancel_selection();
                        textarea.delete_next_char();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('i'),
                        ..
                    } => {
                        textarea.cancel_selection();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('a'),
                        ..
                    } => {
                        textarea.cancel_selection();
                        textarea.move_cursor(CursorMove::Forward);
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('A'),
                        ..
                    } => {
                        textarea.cancel_selection();
                        textarea.move_cursor(CursorMove::End);
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('o'),
                        ..
                    } => {
                        textarea.move_cursor(CursorMove::End);
                        textarea.insert_newline();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('O'),
                        ..
                    } => {
                        textarea.move_cursor(CursorMove::Head);
                        textarea.insert_newline();
                        textarea.move_cursor(CursorMove::Up);
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('I'),
                        ..
                    } => {
                        textarea.cancel_selection();
                        textarea.move_cursor(CursorMove::Head);
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('e'),
                        ctrl: true,
                        ..
                    } => textarea.scroll((1, 0)),
                    Input {
                        key: Key::Char('y'),
                        ctrl: true,
                        ..
                    } => textarea.scroll((-1, 0)),
                    Input {
                        key: Key::Char('d'),
                        ctrl: true,
                        ..
                    } => textarea.scroll(Scrolling::HalfPageDown),
                    Input {
                        key: Key::Char('u'),
                        ctrl: true,
                        ..
                    } => textarea.scroll(Scrolling::HalfPageUp),
                    Input {
                        key: Key::Char('f'),
                        ctrl: true,
                        ..
                    } => textarea.scroll(Scrolling::PageDown),
                    Input {
                        key: Key::Char('b'),
                        ctrl: true,
                        ..
                    } => textarea.scroll(Scrolling::PageUp),
                    Input {
                        key: Key::Char('v'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Normal => {
                        textarea.start_selection();
                        return VimTransition::Mode(VimMode::Visual);
                    }
                    Input {
                        key: Key::Char('V'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Normal => {
                        textarea.move_cursor(CursorMove::Head);
                        textarea.start_selection();
                        textarea.move_cursor(CursorMove::End);
                        return VimTransition::Mode(VimMode::Visual);
                    }
                    Input { key: Key::Esc, .. }
                    | Input {
                        key: Key::Char('v'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Visual => {
                        textarea.cancel_selection();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('g'),
                        ctrl: false,
                        ..
                    } if matches!(
                        self.pending,
                        Input {
                            key: Key::Char('g'),
                            ctrl: false,
                            ..
                        }
                    ) =>
                    {
                        textarea.move_cursor(CursorMove::Top)
                    }
                    Input {
                        key: Key::Char('G'),
                        ctrl: false,
                        ..
                    } => textarea.move_cursor(CursorMove::Bottom),
                    Input {
                        key: Key::Char(c),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Operator(c) => {
                        textarea.move_cursor(CursorMove::Head);
                        textarea.start_selection();
                        let cursor = textarea.cursor();
                        textarea.move_cursor(CursorMove::Down);
                        if cursor == textarea.cursor() {
                            textarea.move_cursor(CursorMove::End);
                        }
                    }
                    Input {
                        key: Key::Char(op @ ('y' | 'd' | 'c')),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Normal => {
                        textarea.start_selection();
                        return VimTransition::Mode(VimMode::Operator(op));
                    }
                    Input {
                        key: Key::Char('y'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Visual => {
                        textarea.move_cursor(CursorMove::Forward);
                        textarea.copy();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('d'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Visual => {
                        textarea.move_cursor(CursorMove::Forward);
                        textarea.cut();
                        return VimTransition::Mode(VimMode::Normal);
                    }
                    Input {
                        key: Key::Char('c'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Visual => {
                        textarea.move_cursor(CursorMove::Forward);
                        textarea.cut();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    Input {
                        key: Key::Char('s'),
                        ctrl: false,
                        ..
                    } if self.mode == VimMode::Visual => {
                        textarea.move_cursor(CursorMove::Forward);
                        textarea.cut();
                        return VimTransition::Mode(VimMode::Insert);
                    }
                    input => return VimTransition::Pending(input),
                }

                match self.mode {
                    VimMode::Operator('y') => {
                        textarea.copy();
                        VimTransition::Mode(VimMode::Normal)
                    }
                    VimMode::Operator('d') => {
                        textarea.cut();
                        VimTransition::Mode(VimMode::Normal)
                    }
                    VimMode::Operator('c') => {
                        textarea.cut();
                        VimTransition::Mode(VimMode::Insert)
                    }
                    _ => VimTransition::Nop,
                }
            }
            VimMode::Insert => match input {
                Input { key: Key::Esc, .. }
                | Input {
                    key: Key::Char('c'),
                    ctrl: true,
                    ..
                } => VimTransition::Mode(VimMode::Normal),
                input => {
                    textarea.input(input);
                    VimTransition::Mode(VimMode::Insert)
                }
            },
        }
    }

    fn cursor_style(&self) -> Style {
        let color = match self.mode {
            VimMode::Normal => Color::Reset,
            VimMode::Insert => Color::LightBlue,
            VimMode::Visual => Color::LightYellow,
            VimMode::Operator(_) => Color::LightGreen,
        };
        Style::default().fg(color).add_modifier(Modifier::REVERSED)
    }
}

pub struct App {
    pub notebook: Notebook,
    pub selected: usize,
    pub status: String,
    notebook_path: Option<PathBuf>,
    checkpoint_paths: Option<CheckpointPaths>,
    pub session: SessionManager,
    mode: AppMode,
    editor: TextArea<'static>,
    vim_enabled: bool,
    vim: Option<VimState>,
}

impl App {
    pub fn new(
        notebook: Notebook,
        notebook_path: Option<PathBuf>,
        session: SessionManager,
        vim_enabled: bool,
    ) -> Self {
        let checkpoint_paths = notebook_path
            .as_ref()
            .map(|path| CheckpointPaths::for_notebook(path));
        let mut app = Self {
            notebook,
            selected: 0,
            status: String::new(),
            notebook_path,
            checkpoint_paths,
            session,
            mode: AppMode::Normal,
            editor: TextArea::default(),
            vim_enabled,
            vim: None,
        };
        app.load_selected_into_editor();
        app.refresh_status();
        app
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let loop_result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        loop_result
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<bool> {
        match self.mode {
            AppMode::Normal => self.handle_normal_mode(key),
            AppMode::Edit => self.handle_edit_mode(key),
        }
    }

    fn handle_normal_mode(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('e') => self.enter_edit_mode(),
            KeyCode::Char('r') => self.run_selected_cell()?,
            KeyCode::Char('b') => self.insert_cell(Language::Bash),
            KeyCode::Char('p') => self.insert_cell(Language::Python),
            KeyCode::Char('J') => self.insert_cell(Language::JavaScript),
            KeyCode::Char('t') => self.insert_cell(Language::TypeScript),
            KeyCode::Char('a') => self.insert_ai_cell(),
            KeyCode::Char('n') => self.insert_text_cell(),
            KeyCode::Char('x') => self.delete_selected_cell(),
            KeyCode::Char('v') => self.toggle_vim_mode(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_all()?
            }
            _ => {}
        }

        Ok(false)
    }

    fn handle_edit_mode(&mut self, key: KeyEvent) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.apply_editor_to_cell();
            self.save_all()?;
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.apply_editor_to_cell();
            self.run_selected_cell()?;
            return Ok(false);
        }

        if self.vim_enabled {
            if key.code == KeyCode::Esc && matches!(self.vim_mode(), Some(VimMode::Normal)) {
                self.apply_editor_to_cell();
                self.mode = AppMode::Normal;
                self.vim = None;
                self.sync_editor_presentation();
                self.refresh_status();
                return Ok(false);
            }

            let input = input_from_key_event(key);
            let Some(vim) = self.vim.clone() else {
                bail!("vim mode enabled without editor state");
            };
            let transition = vim.transition(input, &mut self.editor);
            self.vim = match transition {
                VimTransition::Mode(mode) => Some(VimState::new(mode)),
                VimTransition::Pending(input) => Some(vim.with_pending(input)),
                VimTransition::Nop => Some(vim),
            };
            self.sync_editor_presentation();
            self.refresh_status();
            return Ok(false);
        }

        if key.code == KeyCode::Esc {
            self.apply_editor_to_cell();
            self.mode = AppMode::Normal;
            self.refresh_status();
            return Ok(false);
        }

        self.editor.input(input_from_key_event(key));
        Ok(false)
    }

    fn event_loop<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key_event(key)? {
                        self.save_checkpoint_only()?;
                        return Ok(());
                    }
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(3)])
            .split(frame.area());
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(26),
                Constraint::Percentage(37),
                Constraint::Percentage(37),
            ])
            .split(chunks[0]);

        let items: Vec<ListItem<'_>> = self
            .notebook
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let marker = if index == self.selected { ">" } else { " " };
                let kind = match cell.kind {
                    CellKind::Code => cell.language.fence_name(),
                    CellKind::Ai => "ai",
                    CellKind::Text => "text",
                };
                let content = format!("{marker} {kind} {}", cell.id.0);
                ListItem::new(Line::from(content))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(self.notebook.metadata.title.as_str())
                .borders(Borders::ALL),
        );
        frame.render_widget(list, body[0]);

        self.sync_editor_presentation();
        frame.render_widget(&self.editor, body[1]);

        let output = self.render_output();
        frame.render_widget(output, body[2]);

        let status = Paragraph::new(self.status.as_str())
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(Line::styled(
                self.status_title(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        frame.render_widget(status, chunks[1]);
    }

    fn render_output(&self) -> Paragraph<'_> {
        let text = if let Some(cell) = self.notebook.cells.get(self.selected) {
            if let Some(record) = self.session.latest_record_for_cell(&cell.id.0) {
                let mut lines = vec![format!(
                    "{:?} exit={} lang={}",
                    record.status,
                    record.exit_code,
                    record.language.fence_name()
                )];
                if !record.output.is_empty() {
                    lines.push(record.output.clone());
                }
                if !record.error_output.is_empty() {
                    lines.push(record.error_output.clone());
                }
                lines.join("\n")
            } else if let Some(run) = self
                .session
                .manifest
                .ai_history
                .iter()
                .rev()
                .find(|run| run.prompt_cell_id == cell.id.0)
            {
                let mut lines = vec![format!(
                    "{:?} provider={} model={}",
                    run.status, run.provider_name, run.model_id
                )];
                if !run.response.is_empty() {
                    lines.push(run.response.clone());
                }
                if !run.error_output.is_empty() {
                    lines.push(run.error_output.clone());
                }
                lines.join("\n")
            } else {
                "No output for selected cell.".to_string()
            }
        } else {
            "No selected cell.".to_string()
        };

        Paragraph::new(Text::from(text))
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Output").borders(Borders::ALL))
    }

    fn move_selection(&mut self, delta: isize) {
        if self.notebook.cells.is_empty() {
            return;
        }
        let max_index = self.notebook.cells.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index) as usize;
        self.selected = next;
        self.load_selected_into_editor();
        self.refresh_status();
    }

    fn load_selected_into_editor(&mut self) {
        self.editor = TextArea::default();
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            self.editor = TextArea::from(cell.source.lines().map(|line| line.to_string()));
        }
        if self.mode == AppMode::Edit && self.vim_enabled {
            self.vim = Some(VimState::new(VimMode::Normal));
        }
        self.sync_editor_presentation();
    }

    fn apply_editor_to_cell(&mut self) {
        if let Some(cell) = self.notebook.cells.get_mut(self.selected) {
            cell.source = self.editor.lines().join("\n");
        }
    }

    fn insert_cell(&mut self, language: Language) {
        let next = usize::min(self.selected.saturating_add(1), self.notebook.cells.len());
        self.notebook
            .cells
            .insert(next, Cell::code(language, String::new()));
        self.selected = next;
        self.enter_edit_mode();
        self.status = format!("inserted {} cell", language.fence_name());
    }

    fn insert_ai_cell(&mut self) {
        let next = usize::min(self.selected.saturating_add(1), self.notebook.cells.len());
        self.notebook.cells.insert(next, Cell::ai(String::new()));
        self.selected = next;
        self.enter_edit_mode();
        self.status = "inserted ai cell".to_string();
    }

    fn insert_text_cell(&mut self) {
        let next = usize::min(self.selected.saturating_add(1), self.notebook.cells.len());
        self.notebook.cells.insert(next, Cell::text(String::new()));
        self.selected = next;
        self.enter_edit_mode();
        self.status = "inserted text cell".to_string();
    }

    fn delete_selected_cell(&mut self) {
        if self.notebook.cells.is_empty() {
            return;
        }
        self.notebook.cells.remove(self.selected);
        if self.selected >= self.notebook.cells.len() && !self.notebook.cells.is_empty() {
            self.selected = self.notebook.cells.len() - 1;
        }
        if self.notebook.cells.is_empty() {
            self.notebook.cells.push(Cell::text(String::new()));
            self.selected = 0;
        }
        self.load_selected_into_editor();
        self.status = "deleted selected cell".to_string();
    }

    fn save_all(&mut self) -> Result<()> {
        self.apply_editor_to_cell();
        if let Some(path) = &self.notebook_path {
            NotebookStorage::save_markdown(path, &self.notebook)
                .with_context(|| format!("failed to save {}", path.display()))?;
        }
        self.save_checkpoint_only()?;
        self.status = "saved notebook and checkpoint".to_string();
        Ok(())
    }

    fn save_checkpoint_only(&mut self) -> Result<()> {
        if let Some(paths) = &self.checkpoint_paths {
            CheckpointStorage::save(paths, &self.session.manifest)?;
        }
        Ok(())
    }

    fn run_selected_cell(&mut self) -> Result<()> {
        self.apply_editor_to_cell();
        let record = self.session.run_cell_at(&self.notebook, self.selected)?;
        self.save_checkpoint_only()?;
        self.status = format!(
            "ran {} -> {:?} (exit {})",
            record.cell_id, record.status, record.exit_code
        );
        if record.status == ExecutionStatus::Failed {
            self.mode = AppMode::Normal;
            self.vim = None;
            self.sync_editor_presentation();
        }
        Ok(())
    }

    fn enter_edit_mode(&mut self) {
        self.mode = AppMode::Edit;
        self.vim = if self.vim_enabled {
            Some(VimState::new(VimMode::Normal))
        } else {
            None
        };
        self.load_selected_into_editor();
        self.refresh_status();
    }

    fn toggle_vim_mode(&mut self) {
        self.vim_enabled = !self.vim_enabled;
        self.vim = None;
        self.sync_editor_presentation();
        self.refresh_status();
        self.status = if self.vim_enabled {
            "vim mode enabled for this session".to_string()
        } else {
            "vim mode disabled for this session".to_string()
        };
    }

    fn vim_mode(&self) -> Option<VimMode> {
        self.vim.as_ref().map(|vim| vim.mode)
    }

    fn sync_editor_presentation(&mut self) {
        let title = self.editor_title();
        self.editor
            .set_block(Block::default().title(title).borders(Borders::ALL));
        self.editor
            .set_cursor_line_style(Style::default().add_modifier(Modifier::REVERSED));
        let cursor_style = match self.vim.as_ref() {
            Some(vim) => vim.cursor_style(),
            None => Style::default().add_modifier(Modifier::REVERSED),
        };
        self.editor.set_cursor_style(cursor_style);
    }

    fn editor_title(&self) -> String {
        let prefix = if self.mode == AppMode::Edit {
            "Editing"
        } else {
            "Cell"
        };
        let language = self
            .notebook
            .cells
            .get(self.selected)
            .map(|cell| cell.language.fence_name())
            .unwrap_or("text");
        match self.vim_mode() {
            Some(mode) => format!("{prefix} {language} [VIM {mode}]"),
            None if self.vim_enabled => format!("{prefix} {language} [VIM READY]"),
            None => format!("{prefix} {language}"),
        }
    }

    fn status_title(&self) -> String {
        match self.vim_mode() {
            Some(mode) => format!("Status [{:?} | VIM {mode}]", self.mode),
            None => format!(
                "Status [{:?} | {}]",
                self.mode,
                if self.vim_enabled {
                    "VIM ON"
                } else {
                    "VIM OFF"
                }
            ),
        }
    }

    fn refresh_status(&mut self) {
        self.status = match (self.mode, self.vim_mode(), self.vim_enabled) {
            (AppMode::Normal, _, true) => {
                "normal: j/k move | e edit | v toggle vim | r run | ctrl-s save | b/p/J/t/a/n add | x delete | q quit".to_string()
            }
            (AppMode::Normal, _, false) => {
                "normal: j/k move | e edit | v toggle vim | r run | ctrl-s save | b/p/J/t/a/n add | x delete | q quit".to_string()
            }
            (AppMode::Edit, Some(VimMode::Normal), _) => {
                "edit vim NORMAL: i/a/o enter insert | v visual | Esc exit editor | ctrl-s save | ctrl-r run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Insert), _) => {
                "edit vim INSERT: Esc normal | ctrl-s save | ctrl-r run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Visual), _) => {
                "edit vim VISUAL: y copy | d delete | c change | Esc normal | ctrl-s save | ctrl-r run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Operator(_)), _) => {
                "edit vim OPERATOR: motion applies pending operator | Esc normal | ctrl-s save | ctrl-r run".to_string()
            }
            (AppMode::Edit, None, _) => {
                "edit: Esc finish | ctrl-s save | ctrl-r run".to_string()
            }
        };
    }
}

pub fn should_launch_tui() -> bool {
    io::stdout().is_terminal() && io::stdin().is_terminal() && terminal::size().is_ok()
}

fn input_from_key_event(key: KeyEvent) -> Input {
    Input {
        key: match key.code {
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Enter => Key::Enter,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Tab => Key::Tab,
            KeyCode::Delete => Key::Delete,
            KeyCode::Esc => Key::Esc,
            KeyCode::Char(ch) => Key::Char(ch),
            _ => Key::Null,
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::SessionManager;
    use tempfile::TempDir;

    #[test]
    fn app_editing_updates_selected_cell_source() {
        let notebook =
            Notebook::new("Edit").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert!(app.notebook.cells[0].source.contains('#'));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn app_save_writes_notebook_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("demo.md");
        let notebook = Notebook::new("Save").with_cells(vec![Cell::text("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, Some(path.clone()), session, false);

        app.save_all().unwrap();

        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("# Save"));
    }

    #[test]
    fn vim_mode_can_be_toggled_in_session() {
        let notebook = Notebook::new("Vim").with_cells(vec![Cell::text("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert!(app.vim_enabled);
        assert!(app.status.contains("vim mode enabled"));
    }

    #[test]
    fn vim_mode_enters_normal_and_requires_double_escape_to_exit() {
        let notebook =
            Notebook::new("Vim").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, true);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.vim_mode(), Some(VimMode::Normal));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.vim_mode(), Some(VimMode::Insert));

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.mode, AppMode::Edit);
        assert_eq!(app.vim_mode(), Some(VimMode::Normal));

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.vim_mode(), None);
    }

    #[test]
    fn vim_insert_appends_text_after_entering_insert_mode() {
        let notebook =
            Notebook::new("Vim").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, true);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert!(app.notebook.cells[0].source.ends_with('#'));
    }

    #[test]
    fn vim_delete_command_edits_buffer_in_normal_mode() {
        let notebook = Notebook::new("Vim").with_cells(vec![Cell::code(Language::Python, "abc")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, true);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.notebook.cells[0].source, "bc");
    }

    #[test]
    fn vim_substitute_command_replaces_character_and_enters_insert_mode() {
        let notebook = Notebook::new("Vim").with_cells(vec![Cell::code(Language::Python, "abc")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, true);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.vim_mode(), Some(VimMode::Insert));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.notebook.cells[0].source, "zbc");
    }

    #[test]
    fn vim_visual_substitute_replaces_selection() {
        let notebook = Notebook::new("Vim").with_cells(vec![Cell::code(Language::Python, "abcd")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, true);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.vim_mode(), Some(VimMode::Insert));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.notebook.cells[0].source, "zcd");
    }

    #[test]
    fn inserting_into_empty_notebook_does_not_panic() {
        let notebook = Notebook::new("Empty");
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.notebook.cells.len(), 1);
        assert_eq!(app.selected, 0);
        assert_eq!(app.notebook.cells[0].kind, CellKind::Ai);
    }
}
