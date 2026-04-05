use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tui_textarea::{CursorMove, Input, Key, Scrolling, TextArea};

use crate::core::{
    Cell, CellKind, CellOutput, CellUiState, ExecutionStatus, Language, Notebook,
};
use crate::runtime::SessionManager;
use crate::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};
use crate::theme::Theme;
use crate::tooling::{PythonLspClient, PythonLspStatus, SyntaxHighlighter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppMode {
    Command,
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

    fn cursor_style(&self, theme: &Theme) -> Style {
        match self.mode {
            VimMode::Normal => theme.style("editor.cursor.normal"),
            VimMode::Insert => theme.style("editor.cursor.insert"),
            VimMode::Visual => theme.style("editor.cursor.visual"),
            VimMode::Operator(_) => theme.style("editor.cursor.operator"),
        }
    }
}

#[derive(Clone, Debug)]
struct HitRegion {
    rect: Rect,
    target: HitTarget,
}

#[derive(Clone, Debug)]
enum HitTarget {
    ToolbarSave,
    ToolbarRunAll,
    ToolbarRestart,
    ToolbarAddCode,
    ToolbarAddMarkdown,
    CellSelect(usize),
    CellEditor(usize),
    CellRun(usize),
    CellEdit(usize),
    CellToggleRender(usize),
    CellToggleOutput(usize),
    CellInsertBelow(usize),
    CellDelete(usize),
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
    scroll_offset: u16,
    cell_modes: BTreeMap<String, CellUiState>,
    hit_regions: Vec<HitRegion>,
    active_editor_rect: Option<Rect>,
    drag_selection: bool,
    python_lsp: PythonLspStatus,
    python_lsp_client: Option<PythonLspClient>,
    notebook_area: Rect,
    last_click: Option<(usize, Instant)>,
    theme: Theme,
}

impl App {
    pub fn new(
        notebook: Notebook,
        notebook_path: Option<PathBuf>,
        session: SessionManager,
        vim_enabled: bool,
        theme: Theme,
        startup_notice: Option<String>,
    ) -> Self {
        let checkpoint_paths = notebook_path
            .as_ref()
            .map(|path| CheckpointPaths::for_notebook(path));

        let mut cell_modes = session.manifest.ui_state.cell_modes.clone();
        for cell in &notebook.cells {
            cell_modes.entry(cell.id.0.clone()).or_insert_with(|| CellUiState {
                rendered: cell.kind == CellKind::Markdown,
                output_collapsed: false,
            });
        }

        let selected = usize::min(
            session.manifest.ui_state.selected_cell,
            notebook.cells.len().saturating_sub(1),
        );
        let viewport_row = session.manifest.ui_state.viewport_row as u16;
        let mut app = Self {
            notebook,
            selected,
            status: String::new(),
            notebook_path,
            checkpoint_paths,
            session,
            mode: AppMode::Command,
            editor: TextArea::default(),
            vim_enabled,
            vim: None,
            scroll_offset: viewport_row,
            cell_modes,
            hit_regions: Vec::new(),
            active_editor_rect: None,
            drag_selection: false,
            python_lsp: PythonLspStatus::detect(),
            python_lsp_client: None,
            notebook_area: Rect::default(),
            last_click: None,
            theme,
        };
        app.ensure_notebook_not_empty();
        app.load_selected_into_editor();
        app.refresh_status();
        if let Some(notice) = startup_notice {
            app.status = notice;
        }
        app
    }

    pub fn run(&mut self) -> Result<()> {
        self.activate_python_lsp();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let loop_result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )?;
        terminal.show_cursor()?;

        loop_result
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<bool> {
        match self.mode {
            AppMode::Command => self.handle_command_mode(key),
            AppMode::Edit => self.handle_edit_mode(key),
        }
    }

    fn handle_command_mode(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('e') | KeyCode::Enter => self.enter_edit_mode(),
            KeyCode::Char('r') => self.run_selected_cell()?,
            KeyCode::Char('R') => self.run_all_cells()?,
            KeyCode::Char('c') => self.insert_code_cell(),
            KeyCode::Char('m') => self.insert_markdown_cell(),
            KeyCode::Char('d') | KeyCode::Delete => self.delete_selected_cell(),
            KeyCode::Char('o') => self.toggle_output_for_selected(),
            KeyCode::Char('v') => self.toggle_vim_mode(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_all()?
            }
            KeyCode::PageDown => self.scroll_cells(1),
            KeyCode::PageUp => self.scroll_cells(-1),
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
        if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Enter {
            self.apply_editor_to_cell();
            self.run_selected_cell()?;
            self.mode = AppMode::Command;
            self.vim = None;
            return Ok(false);
        }

        if self.vim_enabled {
            if key.code == KeyCode::Esc && matches!(self.vim_mode(), Some(VimMode::Normal)) {
                self.apply_editor_to_cell();
                self.mode = AppMode::Command;
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
            self.apply_editor_to_cell();
            self.refresh_status();
            return Ok(false);
        }

        if key.code == KeyCode::Esc {
            self.apply_editor_to_cell();
            self.mode = AppMode::Command;
            self.refresh_status();
            return Ok(false);
        }

        self.editor.input(input_from_key_event(key));
        self.apply_editor_to_cell();
        Ok(false)
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.scroll_cells(1);
                return Ok(());
            }
            MouseEventKind::ScrollUp => {
                self.scroll_cells(-1);
                return Ok(());
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = self.hit_test(mouse.column, mouse.row) {
                    match target.clone() {
                        HitTarget::CellEditor(index)
                            if index == self.selected && self.mode == AppMode::Edit =>
                        {
                            self.drag_selection = true;
                            self.activate_hit_target(target, mouse.column, mouse.row)?;
                        }
                        HitTarget::CellSelect(index) | HitTarget::CellEditor(index) => {
                            let is_double_click = self
                                .last_click
                                .as_ref()
                                .map(|(last_index, last_time)| {
                                    *last_index == index
                                        && last_time.elapsed() <= Duration::from_millis(350)
                                })
                                .unwrap_or(false);
                            self.last_click = Some((index, Instant::now()));
                            self.selected = index;
                            self.load_selected_into_editor();
                            self.ensure_selected_visible();
                            if is_double_click {
                                self.enter_edit_mode();
                                if matches!(target, HitTarget::CellEditor(_)) {
                                    if let Some(rect) = self.active_editor_rect {
                                        self.place_editor_cursor(
                                            mouse.column,
                                            mouse.row,
                                            rect,
                                            false,
                                        );
                                    }
                                }
                            }
                        }
                        _ => self.activate_hit_target(target, mouse.column, mouse.row)?,
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag_selection => {
                if let Some(rect) = self.active_editor_rect {
                    self.place_editor_cursor(mouse.column, mouse.row, rect, true);
                    self.apply_editor_to_cell();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_selection = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn event_loop<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key_event(key)? {
                            self.save_checkpoint_only()?;
                            return Ok(());
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse_event(mouse)?,
                    _ => {}
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        self.hit_regions.clear();
        self.active_editor_rect = None;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(frame.area());

        self.draw_toolbar(frame, chunks[0]);
        self.notebook_area = chunks[1];
        self.clamp_scroll_offset();
        self.draw_notebook(frame, chunks[1]);
        let status = Paragraph::new(self.status.as_str())
            .style(self.theme.style("status.body"))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(self.theme.style("cell.border"))
                    .title(Line::styled("Status", self.theme.style("status.title"))),
            );
        frame.render_widget(status, chunks[2]);

    }

    fn draw_toolbar(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let title = Line::from(vec![
            Span::raw(format!(
                "{} | kernel={} | mode={:?} | ",
                self.notebook.metadata.title, self.notebook.metadata.kernelspec.display_name, self.mode
            )),
            Span::styled(self.python_lsp.summary(), self.python_lsp_style()),
            Span::raw(format!(" | theme={}", self.theme.name())),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .style(self.theme.style("toolbar.block"))
            .border_style(self.theme.style("toolbar.border"))
            .title(title);
        frame.render_widget(block, area);
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        let mut spans = vec![
            Span::styled("[Save]", self.theme.style("toolbar.button.save")),
            Span::raw(" "),
            Span::styled("[Run All]", self.theme.style("toolbar.button.run_all")),
            Span::raw(" "),
            Span::styled("[Restart]", self.theme.style("toolbar.button.restart")),
            Span::raw(" "),
            Span::styled("[+ Code]", self.theme.style("toolbar.button.add_code")),
            Span::raw(" "),
            Span::styled("[+ Markdown]", self.theme.style("toolbar.button.add_markdown")),
        ];
        let toolbar = Paragraph::new(Line::from(std::mem::take(&mut spans)));
        frame.render_widget(toolbar, inner);

        let mut x = inner.x;
        for (label, target) in [
            ("[Save]", HitTarget::ToolbarSave),
            ("[Run All]", HitTarget::ToolbarRunAll),
            ("[Restart]", HitTarget::ToolbarRestart),
            ("[+ Code]", HitTarget::ToolbarAddCode),
            ("[+ Markdown]", HitTarget::ToolbarAddMarkdown),
        ] {
            self.hit_regions.push(HitRegion {
                rect: Rect {
                    x,
                    y: inner.y,
                    width: label.len() as u16,
                    height: 1,
                },
                target,
            });
            x += label.len() as u16 + 1;
        }
    }

    fn draw_notebook(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if self.notebook.cells.is_empty() {
            frame.render_widget(
                Paragraph::new("No cells. Use [+ Code] or [+ Markdown].")
                    .style(self.theme.style("notebook.empty"))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .style(self.theme.style("cell.shell"))
                            .border_style(self.theme.style("cell.border"))
                            .title("Notebook"),
                    ),
                area,
            );
            return;
        }

        let mut cursor_y = area.y;
        let start = usize::min(self.scroll_offset as usize, self.notebook.cells.len().saturating_sub(1));
        for index in start..self.notebook.cells.len() {
            let cell = self.notebook.cells[index].clone();
            let remaining = area
                .y
                .saturating_add(area.height)
                .saturating_sub(cursor_y);
            if remaining < 4 {
                break;
            }
            let height = self.cell_height(index).min(remaining);
            let cell_area = Rect {
                x: area.x,
                y: cursor_y,
                width: area.width,
                height,
            };
            self.draw_cell(frame, area, cell_area, index, &cell);
            cursor_y = cursor_y.saturating_add(height.saturating_add(1));
            if cursor_y >= area.y.saturating_add(area.height) {
                break;
            }
        }
    }

    fn draw_cell(
        &mut self,
        frame: &mut Frame<'_>,
        viewport: Rect,
        area: Rect,
        index: usize,
        cell: &Cell,
    ) {
        let selected = index == self.selected;
        let shell_style = if selected {
            self.theme.style("cell.shell.selected")
        } else {
            self.theme.style("cell.shell")
        };
        let border_style = if selected {
            self.theme.style("cell.border.selected")
        } else {
            self.theme.style("cell.border")
        };
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .style(shell_style),
            area,
        );
        let inner = shrink(area, 1);
        if inner.height < 3 || inner.width < 10 {
            return;
        }
        let chrome_style = if selected {
            self.theme.style("cell.prompt.selected")
        } else {
            self.theme.style("cell.prompt")
        };
        let prompt = match cell.kind {
            CellKind::Code => format!("In [{}]:", cell.execution_count.map_or(" ".to_string(), |n| n.to_string())),
            CellKind::Markdown => "[Markdown]".to_string(),
            CellKind::Raw => "[Raw]".to_string(),
            CellKind::Ai => "[AI]".to_string(),
        };
        let rendered = self.cell_mode(cell).rendered && cell.kind == CellKind::Markdown;
        let mut chrome_spans = vec![Span::styled(prompt, chrome_style), Span::raw(" ")];
        if is_executable(cell) {
            chrome_spans.push(Span::styled("[Run]", self.theme.style("cell.button.run")));
            chrome_spans.push(Span::raw(" "));
        }
        chrome_spans.push(Span::styled(
            match cell.kind {
                CellKind::Markdown => {
                    if rendered { "[Edit]" } else { "[Render]" }
                }
                _ => "[Edit]",
            },
            self.theme.style("cell.button.edit"),
        ));
        chrome_spans.push(Span::raw(" "));
        chrome_spans.push(Span::styled("[+]", self.theme.style("cell.button.add")));
        chrome_spans.push(Span::raw(" "));
        chrome_spans.push(Span::styled("[Del]", self.theme.style("cell.button.delete")));
        if !cell.outputs.is_empty() {
            chrome_spans.push(Span::raw(" "));
            chrome_spans.push(Span::styled("[Out]", self.theme.style("cell.button.output")));
        }
        let chrome = Line::from(chrome_spans);
        let chrome_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(chrome), chrome_area);
        self.register_cell_chrome_hits(chrome_area, index, cell, rendered);
        self.hit_regions.push(HitRegion {
            rect: area,
            target: HitTarget::CellSelect(index),
        });

        let input_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: self.input_height(cell, index),
        };
        let inner_input = shrink(input_area, 1);
        let title = match cell.kind {
            CellKind::Code => format!("code {}", cell.language.fence_name()),
            CellKind::Markdown => {
                if rendered {
                    "markdown rendered".to_string()
                } else {
                    "markdown".to_string()
                }
            }
            CellKind::Raw => "raw".to_string(),
            CellKind::Ai => "ai".to_string(),
        };

        if selected && self.mode == AppMode::Edit && (!rendered || cell.kind == CellKind::Code) {
            self.sync_editor_presentation();
            self.editor
                .set_block(Block::default().title(title).borders(Borders::ALL));
            frame.render_widget(&self.editor, input_area);
            self.active_editor_rect = Some(inner_input);
            self.hit_regions.push(HitRegion { rect: input_area, target: HitTarget::CellEditor(index) });
        } else {
            let content = match cell.kind {
                CellKind::Markdown if rendered => render_markdown_block(&cell.source, &self.theme),
                CellKind::Code => render_code_block(cell, &self.theme),
                _ => Text::from(cell.source.clone()),
            };
            let block = Paragraph::new(content)
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border_style)
                        .style(shell_style)
                        .title(title),
                );
            frame.render_widget(block, input_area);
            self.hit_regions.push(HitRegion {
                rect: area,
                target: HitTarget::CellSelect(index),
            });
        }

        let output_collapsed = self.cell_mode(cell).output_collapsed;
        if !cell.outputs.is_empty() && !output_collapsed {
            let output_area = Rect {
                x: inner.x + 2,
                y: inner.y + 1 + input_area.height,
                width: inner.width.saturating_sub(2),
                height: self.output_height(cell).min(inner.height.saturating_sub(1 + input_area.height)),
            };
            let output = Paragraph::new(render_output_block(cell, &self.theme))
                .style(self.theme.style("output.block"))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .style(self.theme.style("output.block"))
                        .border_style(self.theme.style("output.border"))
                        .title("Output"),
                );
            if output_area.height > 0 && output_area.y < viewport.y + viewport.height {
                frame.render_widget(output, output_area);
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.notebook.cells.is_empty() {
            return;
        }
        let max_index = self.notebook.cells.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index) as usize;
        self.selected = next;
        self.load_selected_into_editor();
        self.ensure_selected_visible();
    }

    fn load_selected_into_editor(&mut self) {
        self.editor = TextArea::default();
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            self.editor = TextArea::from(cell.source.lines().map(|line| line.to_string()));
        }
        self.editor
            .set_cursor_line_style(self.theme.style("editor.cursor_line"));
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

    fn insert_code_cell(&mut self) {
        let next = usize::min(self.selected.saturating_add(1), self.notebook.cells.len());
        self.notebook
            .cells
            .insert(next, Cell::code(Language::Python, String::new()));
        self.cell_modes
            .insert(self.notebook.cells[next].id.0.clone(), CellUiState::default());
        self.selected = next;
        self.enter_edit_mode();
        self.status = "inserted code cell".to_string();
    }

    fn insert_markdown_cell(&mut self) {
        let next = usize::min(self.selected.saturating_add(1), self.notebook.cells.len());
        self.notebook.cells.insert(next, Cell::markdown(String::new()));
        self.cell_modes.insert(
            self.notebook.cells[next].id.0.clone(),
            CellUiState {
                rendered: false,
                output_collapsed: false,
            },
        );
        self.selected = next;
        self.enter_edit_mode();
        self.status = "inserted markdown cell".to_string();
    }

    fn delete_selected_cell(&mut self) {
        if self.notebook.cells.is_empty() {
            return;
        }
        let removed = self.notebook.cells.remove(self.selected);
        self.cell_modes.remove(&removed.id.0);
        if self.selected >= self.notebook.cells.len() && !self.notebook.cells.is_empty() {
            self.selected = self.notebook.cells.len() - 1;
        }
        self.ensure_notebook_not_empty();
        self.load_selected_into_editor();
        self.status = "deleted selected cell".to_string();
    }

    fn ensure_notebook_not_empty(&mut self) {
        if self.notebook.cells.is_empty() {
            self.notebook.cells.push(Cell::markdown(String::new()));
            self.selected = 0;
        }
    }

    fn save_all(&mut self) -> Result<()> {
        self.apply_editor_to_cell();
        self.sync_manifest_ui_state();
        if let Some(path) = &self.notebook_path {
            NotebookStorage::save(path, &self.notebook)
                .with_context(|| format!("failed to save {}", path.display()))?;
        }
        self.save_checkpoint_only()?;
        self.status = "saved notebook and checkpoint".to_string();
        Ok(())
    }

    fn save_checkpoint_only(&mut self) -> Result<()> {
        self.sync_manifest_ui_state();
        if let Some(paths) = &self.checkpoint_paths {
            CheckpointStorage::save(paths, &self.session.manifest)?;
        }
        Ok(())
    }

    fn sync_manifest_ui_state(&mut self) {
        self.session.manifest.ui_state.selected_cell = self.selected;
        self.session.manifest.ui_state.viewport_row = self.scroll_offset as usize;
        self.session.manifest.ui_state.cell_modes = self.cell_modes.clone();
    }

    fn run_selected_cell(&mut self) -> Result<()> {
        self.apply_editor_to_cell();
        let record = self.session.run_cell_at(&mut self.notebook, self.selected)?;
        self.save_checkpoint_only()?;
        self.status = format!(
            "ran {} -> {:?} (exit {})",
            record.cell_id, record.status, record.exit_code
        );
        if record.status == ExecutionStatus::Failed {
            self.mode = AppMode::Command;
            self.vim = None;
            self.sync_editor_presentation();
        }
        Ok(())
    }

    fn run_all_cells(&mut self) -> Result<()> {
        self.apply_editor_to_cell();
        for index in 0..self.notebook.cells.len() {
            if matches!(self.notebook.cells[index].kind, CellKind::Code | CellKind::Ai) {
                self.selected = index;
                self.session.run_cell_at(&mut self.notebook, index)?;
            }
        }
        self.save_checkpoint_only()?;
        self.status = "ran all executable cells".to_string();
        Ok(())
    }

    fn restart_runtime(&mut self) -> Result<()> {
        self.session.restart_all()?;
        self.status = "restarted notebook runtime".to_string();
        Ok(())
    }

    fn enter_edit_mode(&mut self) {
        self.mode = AppMode::Edit;
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            if cell.kind == CellKind::Markdown {
                self.cell_modes
                    .entry(cell.id.0.clone())
                    .or_default()
                    .rendered = false;
            }
        }
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
        let status = if self.vim_enabled {
            "vim mode enabled for this session".to_string()
        } else {
            "vim mode disabled for this session".to_string()
        };
        self.refresh_status();
        self.status = status;
    }

    fn toggle_output_for_selected(&mut self) {
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            let mode = self.cell_modes.entry(cell.id.0.clone()).or_default();
            mode.output_collapsed = !mode.output_collapsed;
        }
    }

    fn cell_mode(&self, cell: &Cell) -> CellUiState {
        self.cell_modes
            .get(&cell.id.0)
            .cloned()
            .unwrap_or_else(|| CellUiState {
                rendered: cell.kind == CellKind::Markdown,
                output_collapsed: false,
            })
    }

    fn activate_python_lsp(&mut self) {
        if self.python_lsp_client.is_some() {
            return;
        }
        if let Ok((client, status)) = PythonLspClient::activate(&self.python_lsp) {
            self.python_lsp = status;
            self.python_lsp_client = Some(client);
        }
    }

    fn vim_mode(&self) -> Option<VimMode> {
        self.vim.as_ref().map(|vim| vim.mode)
    }

    fn sync_editor_presentation(&mut self) {
        self.editor
            .set_cursor_line_style(self.theme.style("editor.cursor_line"));
        let cursor_style = match self.vim.as_ref() {
            Some(vim) => vim.cursor_style(&self.theme),
            None => self.theme.style("editor.cursor.normal"),
        };
        self.editor.set_cursor_style(cursor_style);
    }

    fn refresh_status(&mut self) {
        self.status = match (self.mode, self.vim_mode()) {
            (AppMode::Command, _) => "command: click cells/buttons | e edit | r run | R run all | c code | m markdown | d delete | ctrl-s save | q quit".to_string(),
            (AppMode::Edit, Some(VimMode::Normal)) => {
                "edit vim NORMAL: i insert | Esc exit editor | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Insert)) => {
                "edit vim INSERT: Esc normal | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Visual)) => {
                "edit vim VISUAL: y copy | d delete | c change | Esc normal".to_string()
            }
            (AppMode::Edit, Some(VimMode::Operator(_))) => {
                "edit vim OPERATOR: motion applies pending operator".to_string()
            }
            (AppMode::Edit, None) => {
                "edit: Esc finish | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
        };
    }

    fn hit_test(&self, column: u16, row: u16) -> Option<HitTarget> {
        self.hit_regions
            .iter()
            .find(|region| contains(region.rect, column, row))
            .map(|region| region.target.clone())
    }

    fn activate_hit_target(&mut self, target: HitTarget, column: u16, row: u16) -> Result<()> {
        match target {
            HitTarget::ToolbarSave => self.save_all()?,
            HitTarget::ToolbarRunAll => self.run_all_cells()?,
            HitTarget::ToolbarRestart => self.restart_runtime()?,
            HitTarget::ToolbarAddCode => self.insert_code_cell(),
            HitTarget::ToolbarAddMarkdown => self.insert_markdown_cell(),
            HitTarget::CellSelect(index) => {
                self.selected = index;
                self.load_selected_into_editor();
                self.ensure_selected_visible();
            }
            HitTarget::CellEditor(index) => {
                self.selected = index;
                self.enter_edit_mode();
                if let Some(rect) = self.active_editor_rect {
                    self.place_editor_cursor(column, row, rect, false);
                }
            }
            HitTarget::CellRun(index) => {
                self.selected = index;
                self.run_selected_cell()?;
            }
            HitTarget::CellEdit(index) => {
                self.selected = index;
                self.enter_edit_mode();
            }
            HitTarget::CellToggleRender(index) => {
                self.selected = index;
                if let Some(cell) = self.notebook.cells.get(index) {
                    let mode = self.cell_modes.entry(cell.id.0.clone()).or_default();
                    mode.rendered = !mode.rendered;
                }
            }
            HitTarget::CellToggleOutput(index) => {
                self.selected = index;
                self.toggle_output_for_selected();
            }
            HitTarget::CellInsertBelow(index) => {
                self.selected = index;
                if matches!(self.notebook.cells[index].kind, CellKind::Markdown) {
                    self.insert_markdown_cell();
                } else {
                    self.insert_code_cell();
                }
            }
            HitTarget::CellDelete(index) => {
                self.selected = index;
                self.delete_selected_cell();
            }
        }
        Ok(())
    }

    fn place_editor_cursor(&mut self, column: u16, row: u16, rect: Rect, selecting: bool) {
        let local_row = row.saturating_sub(rect.y);
        let local_col = column.saturating_sub(rect.x);
        if selecting && !self.editor.is_selecting() {
            self.editor.start_selection();
        } else if !selecting {
            self.editor.cancel_selection();
        }
        self.editor.move_cursor(CursorMove::Jump(local_row, local_col));
        self.sync_editor_presentation();
    }

    fn register_cell_chrome_hits(&mut self, chrome_area: Rect, index: usize, cell: &Cell, rendered: bool) {
        let mut labels = Vec::new();
        if is_executable(cell) {
            labels.push(("[Run]", HitTarget::CellRun(index)));
        }
        labels.push((
            match cell.kind {
                CellKind::Markdown => {
                    if rendered { "[Edit]" } else { "[Render]" }
                }
                _ => "[Edit]",
            },
            if cell.kind == CellKind::Markdown {
                HitTarget::CellToggleRender(index)
            } else {
                HitTarget::CellEdit(index)
            },
        ));
        labels.push(("[+]", HitTarget::CellInsertBelow(index)));
        labels.push(("[Del]", HitTarget::CellDelete(index)));
        if !cell.outputs.is_empty() {
            labels.push(("[Out]", HitTarget::CellToggleOutput(index)));
        }

        let mut x = chrome_area.x + prompt_width(cell);
        for (label, target) in labels {
            self.hit_regions.push(HitRegion {
                rect: Rect {
                    x,
                    y: chrome_area.y,
                    width: label.len() as u16,
                    height: 1,
                },
                target,
            });
            x += label.len() as u16 + 1;
        }
        self.hit_regions.push(HitRegion {
            rect: Rect {
                x: chrome_area.x,
                y: chrome_area.y,
                width: prompt_width(cell).saturating_sub(1),
                height: 1,
            },
            target: HitTarget::CellSelect(index),
        });
    }

    fn cell_height(&self, index: usize) -> u16 {
        let Some(cell) = self.notebook.cells.get(index) else {
            return 3;
        };
        let input = self.input_height(cell, index);
        let output = if !cell.outputs.is_empty() && !self.cell_mode(cell).output_collapsed {
            self.output_height(cell)
        } else {
            0
        };
        2 + 1 + input + output
    }

    fn input_height(&self, cell: &Cell, index: usize) -> u16 {
        let lines = if index == self.selected && self.mode == AppMode::Edit {
            self.editor.lines().len().max(1)
        } else {
            cell.source.lines().count().max(1)
        };
        (lines as u16).min(10) + 2
    }

    fn output_height(&self, cell: &Cell) -> u16 {
        let lines = render_output_block(cell, &self.theme).lines.len().max(1);
        (lines as u16).min(8) + 2
    }

    fn scroll_cells(&mut self, delta: isize) {
        if self.notebook.cells.is_empty() {
            return;
        }
        let max_index = self.notebook.cells.len().saturating_sub(1) as isize;
        let next = (self.scroll_offset as isize + delta).clamp(0, max_index) as u16;
        self.scroll_offset = next;
    }

    fn clamp_scroll_offset(&mut self) {
        if self.notebook.cells.is_empty() {
            self.scroll_offset = 0;
            return;
        }
        let max = self.notebook.cells.len().saturating_sub(1) as u16;
        self.scroll_offset = self.scroll_offset.min(max);
    }

    fn ensure_selected_visible(&mut self) {
        self.clamp_scroll_offset();
        let top = self.scroll_offset as usize;
        if self.selected < top {
            self.scroll_offset = self.selected as u16;
            return;
        }
        let mut cursor_y = self.notebook_area.y;
        let bottom = self.notebook_area.y.saturating_add(self.notebook_area.height);
        for index in top..=self.selected {
            let height = self.cell_height(index).saturating_add(1);
            if cursor_y.saturating_add(height) > bottom {
                self.scroll_offset = self.selected as u16;
                break;
            }
            cursor_y = cursor_y.saturating_add(height);
        }
    }

    fn python_lsp_style(&self) -> Style {
        match self.python_lsp {
            PythonLspStatus::Active { .. } => self.theme.style("lsp.active"),
            PythonLspStatus::Available { .. } => self.theme.style("lsp.available"),
            PythonLspStatus::Unavailable => self.theme.style("lsp.unavailable"),
        }
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

fn render_markdown_block(source: &str, theme: &Theme) -> Text<'static> {
    let mut lines = Vec::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            lines.push(Line::from(vec![Span::styled(
                rest.to_string(),
                theme.style("markdown.heading1"),
            )]));
        } else if let Some(rest) = line.strip_prefix("## ") {
            lines.push(Line::from(vec![Span::styled(
                rest.to_string(),
                theme.style("markdown.heading2"),
            )]));
        } else {
            lines.push(Line::from(line.to_string()));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(String::new()));
    }
    Text::from(lines)
}

fn render_code_block(cell: &Cell, theme: &Theme) -> Text<'static> {
    SyntaxHighlighter::highlight_with_theme(cell.language, &cell.source, theme)
}

fn render_output_block(cell: &Cell, theme: &Theme) -> Text<'static> {
    let mut lines = Vec::new();
    for output in &cell.outputs {
        match output {
            CellOutput::Stream { name, text } => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{name}:"),
                    theme.style("output.stream.label"),
                )]));
                for line in text.lines() {
                    lines.push(Line::from(line.to_string()));
                }
            }
            CellOutput::ExecuteResult {
                execution_count,
                data,
                ..
            } => {
                lines.push(Line::from(vec![Span::styled(
                    format!("Out [{execution_count}]:"),
                    theme.style("output.result.label"),
                )]));
                if let Some(value) = data.get("text/plain") {
                    for line in value.as_str().unwrap_or_default().lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                }
            }
            CellOutput::DisplayData { data, .. } => {
                if let Some(value) = data.get("text/plain") {
                    for line in value.as_str().unwrap_or_default().lines() {
                        lines.push(Line::from(line.to_string()));
                    }
                }
            }
            CellOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{ename}: {evalue}"),
                    theme.style("output.error.label"),
                )]));
                for line in traceback {
                    lines.push(Line::from(Span::styled(
                        line.clone(),
                        theme.style("output.error.trace"),
                    )));
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("No output."));
    }
    Text::from(lines)
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn is_executable(cell: &Cell) -> bool {
    matches!(cell.kind, CellKind::Code | CellKind::Ai)
}

fn prompt_width(cell: &Cell) -> u16 {
    let prompt = match cell.kind {
        CellKind::Code => format!(
            "In [{}]:",
            cell.execution_count
                .map_or(" ".to_string(), |n| n.to_string())
        ),
        CellKind::Markdown => "[Markdown]".to_string(),
        CellKind::Raw => "[Raw]".to_string(),
        CellKind::Ai => "[AI]".to_string(),
    };
    prompt.len() as u16 + 2
}

fn shrink(rect: Rect, amount: u16) -> Rect {
    Rect {
        x: rect.x.saturating_add(amount),
        y: rect.y.saturating_add(amount),
        width: rect.width.saturating_sub(amount * 2),
        height: rect.height.saturating_sub(amount * 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::SessionManager;
    use crate::theme::Theme;
    use tempfile::TempDir;

    #[test]
    fn app_editing_updates_selected_cell_source() {
        let notebook =
            Notebook::new("Edit").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

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
        assert_eq!(app.mode, AppMode::Command);
    }

    #[test]
    fn app_save_writes_ipynb_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("demo.smd");
        let notebook = Notebook::new("Save").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(
            notebook,
            Some(path.clone()),
            session,
            false,
            Theme::default_theme(),
            None,
        );

        app.save_all().unwrap();

        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("strata:format"));
        assert!(saved.contains("title=\"Save\""));
    }

    #[test]
    fn vim_mode_can_be_toggled_in_session() {
        let notebook = Notebook::new("Vim").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert!(app.vim_enabled);
        assert!(app.status.contains("vim mode enabled"));
    }

    #[test]
    fn run_selected_cell_updates_inline_outputs() {
        let notebook = Notebook::new("Run")
            .with_cells(vec![Cell::code(Language::Python, "print('hello')")]);
        let mut session = SessionManager::new(&notebook);
        session.register_default_kernels().unwrap();
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.run_selected_cell().unwrap();

        assert_eq!(app.notebook.cells[0].execution_count, Some(1));
        assert!(!app.notebook.cells[0].outputs.is_empty());
    }

    #[test]
    fn inserting_into_empty_notebook_creates_markdown_cell() {
        let notebook = Notebook::new("Empty");
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.insert_markdown_cell();

        assert_eq!(app.notebook.cells.len(), 2);
        assert_eq!(app.notebook.cells[1].kind, CellKind::Markdown);
    }

    #[test]
    fn markdown_cells_are_not_executable() {
        let notebook = Notebook::new("Doc").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        let result = app.run_selected_cell();

        assert!(result.is_err());
    }

    #[test]
    fn selection_scrolls_to_keep_selected_cell_visible() {
        let cells = (0..8)
            .map(|i| Cell::markdown(format!("cell {i}")))
            .collect::<Vec<_>>();
        let notebook = Notebook::new("Scroll").with_cells(cells);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);
        app.notebook_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 8,
        };

        app.move_selection(5);

        assert_eq!(app.selected, 5);
        assert!(app.scroll_offset > 0);
    }
}
