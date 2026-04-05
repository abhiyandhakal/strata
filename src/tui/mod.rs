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
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tui_textarea::{CursorMove, Input, Key, Scrolling, TextArea};

use crate::clipboard::{Clipboard, ClipboardResult};
use crate::core::{
    Cell, CellKind, CellOutput, CellUiState, ExecutionStatus, KernelKind, Language, Notebook,
};
use crate::runtime::{EnvironmentOption, SessionManager, discover_environments};
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
    ToolbarCycleKernel,
    ToolbarCycleEnvironment,
    ToolbarAddCode,
    ToolbarAddMarkdown,
    CellSelect(usize),
    CellEditor(usize),
    CellOutput(usize),
    CellRun(usize),
    CellEdit(usize),
    CellToggleBody(usize),
    CellToggleRender(usize),
    CellToggleOutput(usize),
    CellOpenImage(usize),
    CellInsertBelow(usize),
    CellDelete(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyTarget {
    CellBody,
    CellOutput,
}

#[derive(Clone, Debug)]
struct ContentRegion {
    rect: Rect,
    cell_index: usize,
    target: CopyTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TextPoint {
    row: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseTextSelection {
    cell_index: usize,
    target: CopyTarget,
    anchor: TextPoint,
    focus: TextPoint,
}

impl MouseTextSelection {
    fn normalized(self) -> (TextPoint, TextPoint) {
        if self.anchor.row < self.focus.row
            || (self.anchor.row == self.focus.row && self.anchor.col <= self.focus.col)
        {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    fn is_empty(self) -> bool {
        self.anchor == self.focus
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
    scroll_offset: u16,
    cell_modes: BTreeMap<String, CellUiState>,
    hit_regions: Vec<HitRegion>,
    content_regions: Vec<ContentRegion>,
    active_editor_rect: Option<Rect>,
    drag_selection: bool,
    drag_content_selection: bool,
    python_lsp: PythonLspStatus,
    python_lsp_client: Option<PythonLspClient>,
    notebook_area: Rect,
    last_click: Option<(usize, Instant)>,
    theme: Theme,
    editor_row_offset: usize,
    copy_target: CopyTarget,
    command_prefix: Option<char>,
    clipboard: Clipboard,
    mouse_text_selection: Option<MouseTextSelection>,
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
        Self::new_with_clipboard(
            notebook,
            notebook_path,
            session,
            vim_enabled,
            theme,
            startup_notice,
            Clipboard::system(),
        )
    }

    fn new_with_clipboard(
        notebook: Notebook,
        notebook_path: Option<PathBuf>,
        session: SessionManager,
        vim_enabled: bool,
        theme: Theme,
        startup_notice: Option<String>,
        clipboard: Clipboard,
    ) -> Self {
        let checkpoint_paths = notebook_path
            .as_ref()
            .map(|path| CheckpointPaths::for_notebook(path));

        let mut cell_modes = session.manifest.ui_state.cell_modes.clone();
        for cell in &notebook.cells {
            cell_modes.entry(cell.id.0.clone()).or_insert_with(|| CellUiState {
                rendered: cell.kind == CellKind::Markdown,
                body_collapsed: false,
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
            content_regions: Vec::new(),
            active_editor_rect: None,
            drag_selection: false,
            drag_content_selection: false,
            python_lsp: PythonLspStatus::detect(),
            python_lsp_client: None,
            notebook_area: Rect::default(),
            last_click: None,
            theme,
            editor_row_offset: 0,
            copy_target: CopyTarget::CellBody,
            command_prefix: None,
            clipboard,
            mouse_text_selection: None,
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.copy_current_target()?;
            return Ok(false);
        }
        if self.command_prefix == Some('g') {
            self.command_prefix = None;
            if matches!(key.code, KeyCode::Char('y')) {
                self.copy_selected_output()?;
                return Ok(false);
            }
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('e') | KeyCode::Enter => self.enter_edit_mode(),
            KeyCode::Char('K') => self.cycle_kernel()?,
            KeyCode::Char('E') => self.cycle_environment()?,
            KeyCode::Char('x') => self.open_selected_image()?,
            KeyCode::Char('r') => self.run_selected_cell()?,
            KeyCode::Char('R') => self.run_all_cells()?,
            KeyCode::Char('y') => self.copy_current_target()?,
            KeyCode::Char('Y') => self.copy_selected_cell_block()?,
            KeyCode::Char('g') => self.command_prefix = Some('g'),
            KeyCode::Char('c') => self.insert_code_cell(),
            KeyCode::Char('m') => self.insert_markdown_cell(),
            KeyCode::Char('d') | KeyCode::Delete => self.delete_selected_cell(),
            KeyCode::Char('o') => self.toggle_output_for_selected(),
            KeyCode::Char('z') => self.toggle_body_for_selected(),
            KeyCode::Char('v') => self.toggle_vim_mode(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_all()?
            }
            KeyCode::PageDown => self.scroll_cells(self.page_scroll_amount()),
            KeyCode::PageUp => self.scroll_cells(-self.page_scroll_amount()),
            _ => {}
        }

        Ok(false)
    }

    fn handle_edit_mode(&mut self, key: KeyEvent) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char('z') {
            self.apply_editor_to_cell();
            self.mode = AppMode::Command;
            self.vim = None;
            self.toggle_body_for_selected();
            self.refresh_status();
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.copy_editor_selection()?;
            return Ok(false);
        }
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
            let prior_mode = vim.mode;
            let copied_input = input.clone();
            let transition = vim.transition(input, &mut self.editor);
            self.vim = match &transition {
                VimTransition::Mode(mode) => Some(VimState::new(*mode)),
                VimTransition::Pending(pending) => Some(vim.with_pending(pending.clone())),
                VimTransition::Nop => Some(vim),
            };
            if should_copy_vim_selection(prior_mode, copied_input, &transition) {
                self.copy_yank_buffer("editor selection")?;
            }
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
                        HitTarget::CellEditor(index) if self.mode == AppMode::Command => {
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
                            self.copy_target = CopyTarget::CellBody;
                            self.clear_mouse_text_selection();
                            self.load_selected_into_editor();
                            self.ensure_selected_visible();
                            if is_double_click {
                                self.enter_edit_mode();
                            } else if let Some(region) =
                                self.content_region_at(mouse.column, mouse.row).cloned()
                            {
                                let point =
                                    text_point_from_mouse(&region, mouse.column, mouse.row, self);
                                self.mouse_text_selection = Some(MouseTextSelection {
                                    cell_index: region.cell_index,
                                    target: CopyTarget::CellBody,
                                    anchor: point,
                                    focus: point,
                                });
                                self.drag_content_selection = true;
                            }
                        }
                        HitTarget::CellOutput(index) if self.mode == AppMode::Command => {
                            self.selected = index;
                            self.copy_target = CopyTarget::CellOutput;
                            self.clear_mouse_text_selection();
                            self.load_selected_into_editor();
                            self.ensure_selected_visible();
                            if let Some(region) =
                                self.content_region_at(mouse.column, mouse.row).cloned()
                            {
                                let point =
                                    text_point_from_mouse(&region, mouse.column, mouse.row, self);
                                self.mouse_text_selection = Some(MouseTextSelection {
                                    cell_index: region.cell_index,
                                    target: CopyTarget::CellOutput,
                                    anchor: point,
                                    focus: point,
                                });
                                self.drag_content_selection = true;
                            }
                        }
                        HitTarget::CellSelect(index) => {
                            self.last_click = Some((index, Instant::now()));
                            self.selected = index;
                            self.copy_target = CopyTarget::CellBody;
                            self.clear_mouse_text_selection();
                            self.load_selected_into_editor();
                            self.ensure_selected_visible();
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
            MouseEventKind::Drag(MouseButton::Left) if self.drag_content_selection => {
                if let Some(region) = self.content_region_at(mouse.column, mouse.row).cloned() {
                    let point = text_point_from_mouse(&region, mouse.column, mouse.row, self);
                    if let Some(selection) = self.mouse_text_selection.as_mut() {
                        if selection.cell_index == region.cell_index && selection.target == region.target
                        {
                            selection.focus = point;
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_selection = false;
                self.drag_content_selection = false;
                if matches!(self.mouse_text_selection, Some(selection) if selection.is_empty()) {
                    self.mouse_text_selection = None;
                }
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
        self.content_regions.clear();
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
        let display_title = self.notebook.display_title(self.notebook_path.as_deref());
        let kernel_label = self.notebook.metadata.runtime.kernel.display_name();
        let environment_label = self.current_environment_label();
        let title = Line::from(vec![
            Span::raw(format!(
                "{} | kernel={} | env={} | mode={:?} | ",
                display_title, kernel_label, environment_label, self.mode
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
        let kernel_button = format!("[Kernel: {kernel_label}]");
        let environment_button = format!("[Env: {environment_label}]");
        let mut spans = vec![
            Span::styled("[Save]", self.theme.style("toolbar.button.save")),
            Span::raw(" "),
            Span::styled("[Run All]", self.theme.style("toolbar.button.run_all")),
            Span::raw(" "),
            Span::styled("[Restart]", self.theme.style("toolbar.button.restart")),
            Span::raw(" "),
            Span::styled(
                kernel_button.clone(),
                self.theme.style("toolbar.button.add_code"),
            ),
            Span::raw(" "),
            Span::styled(
                environment_button.clone(),
                self.theme.style("toolbar.button.add_markdown"),
            ),
            Span::raw(" "),
            Span::styled("[+ Code]", self.theme.style("toolbar.button.add_code")),
            Span::raw(" "),
            Span::styled("[+ Markdown]", self.theme.style("toolbar.button.add_markdown")),
        ];
        let toolbar = Paragraph::new(Line::from(std::mem::take(&mut spans)));
        frame.render_widget(toolbar, inner);

        let mut x = inner.x;
        for (label, target) in vec![
            ("[Save]".to_string(), HitTarget::ToolbarSave),
            ("[Run All]".to_string(), HitTarget::ToolbarRunAll),
            ("[Restart]".to_string(), HitTarget::ToolbarRestart),
            (kernel_button, HitTarget::ToolbarCycleKernel),
            (environment_button, HitTarget::ToolbarCycleEnvironment),
            ("[+ Code]".to_string(), HitTarget::ToolbarAddCode),
            ("[+ Markdown]".to_string(), HitTarget::ToolbarAddMarkdown),
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
        let body_collapsed = self.cell_mode(cell).body_collapsed;
        let mut chrome_spans = vec![Span::styled(prompt, chrome_style), Span::raw(" ")];
        if self.is_cell_runnable(cell) {
            chrome_spans.push(Span::styled("[Run]", self.theme.style("cell.button.run")));
            chrome_spans.push(Span::raw(" "));
        } else if matches!(cell.kind, CellKind::Code) {
            chrome_spans.push(Span::styled("[Unsupported]", self.theme.style("output.error.label")));
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
        chrome_spans.push(Span::styled(
            if body_collapsed { "[Unfold]" } else { "[Fold]" },
            self.theme.style("cell.button.edit"),
        ));
        chrome_spans.push(Span::raw(" "));
        chrome_spans.push(Span::styled("[Del]", self.theme.style("cell.button.delete")));
        if !cell.outputs.is_empty() {
            chrome_spans.push(Span::raw(" "));
            chrome_spans.push(Span::styled(
                if self.cell_mode(cell).output_collapsed {
                    "[Show Out]"
                } else {
                    "[Hide Out]"
                },
                self.theme.style("cell.button.output"),
            ));
            if self.first_image_output(cell).is_some() {
                chrome_spans.push(Span::raw(" "));
                chrome_spans.push(Span::styled("[Open]", self.theme.style("toolbar.button.add_markdown")));
            }
        }
        let chrome = Line::from(chrome_spans);
        let chrome_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        self.hit_regions.push(HitRegion {
            rect: area,
            target: HitTarget::CellSelect(index),
        });
        frame.render_widget(Paragraph::new(chrome), chrome_area);
        self.register_cell_chrome_hits(chrome_area, index, cell, rendered);

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

        if body_collapsed {
            let collapsed = Paragraph::new(Line::from(vec![Span::styled(
                "... cell body collapsed ...",
                self.theme.style("output.stream.label"),
            )]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .style(shell_style)
                    .title(format!("{title} (collapsed)")),
            );
            frame.render_widget(collapsed, input_area);
        } else if selected && self.mode == AppMode::Edit && (!rendered || cell.kind == CellKind::Code) {
            self.sync_editor_presentation();
            if cell.kind == CellKind::Code {
                self.render_code_editor(frame, input_area, inner_input, &title, cell);
            } else {
                self.editor
                    .set_block(Block::default().title(title).borders(Borders::ALL));
                frame.render_widget(&self.editor, input_area);
            }
            self.active_editor_rect = Some(inner_input);
            self.hit_regions.push(HitRegion { rect: input_area, target: HitTarget::CellEditor(index) });
        } else {
            let content = match cell.kind {
                CellKind::Markdown if rendered => render_markdown_block(&cell.source, &self.theme),
                CellKind::Code => render_code_block(cell, &self.theme),
                _ => Text::from(cell.source.clone()),
            };
            let content_area = shrink(input_area, 1);
            let content = if selected && self.copy_target == CopyTarget::CellBody {
                apply_mouse_selection_to_text(
                    content,
                    self.mouse_text_selection_for(index, CopyTarget::CellBody),
                    self.theme.style("cell.prompt.selected"),
                )
            } else {
                content
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
                rect: input_area,
                target: HitTarget::CellEditor(index),
            });
            if content_area.width > 0 && content_area.height > 0 {
                self.content_regions.push(ContentRegion {
                    rect: content_area,
                    cell_index: index,
                    target: CopyTarget::CellBody,
                });
            }
        }

        let output_collapsed = self.cell_mode(cell).output_collapsed;
        if !cell.outputs.is_empty() && !output_collapsed {
            let output_area = Rect {
                x: inner.x + 2,
                y: inner.y + 1 + input_area.height,
                width: inner.width.saturating_sub(2),
                height: self.output_height(cell).min(inner.height.saturating_sub(1 + input_area.height)),
            };
            let output_selected = selected && self.copy_target == CopyTarget::CellOutput;
            let output_text = render_output_block(cell, &self.theme);
            let output_text = if output_selected {
                apply_mouse_selection_to_text(
                    output_text,
                    self.mouse_text_selection_for(index, CopyTarget::CellOutput),
                    self.theme.style("cell.prompt.selected"),
                )
            } else {
                output_text
            };
            let output = Paragraph::new(output_text)
                .style(if output_selected {
                    self.theme.style("cell.shell.selected")
                } else {
                    self.theme.style("output.block")
                })
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .style(if output_selected {
                            self.theme.style("cell.shell.selected")
                        } else {
                            self.theme.style("output.block")
                        })
                        .border_style(if output_selected {
                            self.theme.style("cell.border.selected")
                        } else {
                            self.theme.style("output.border")
                        })
                        .title("Output"),
                );
            if output_area.height > 0 && output_area.y < viewport.y + viewport.height {
                frame.render_widget(output, output_area);
                self.hit_regions.push(HitRegion {
                    rect: output_area,
                    target: HitTarget::CellOutput(index),
                });
                let content_area = shrink(output_area, 1);
                if content_area.width > 0 && content_area.height > 0 {
                    self.content_regions.push(ContentRegion {
                        rect: content_area,
                        cell_index: index,
                        target: CopyTarget::CellOutput,
                    });
                }
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
        self.copy_target = CopyTarget::CellBody;
        self.clear_mouse_text_selection();
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
        self.sync_editor_row_offset(usize::MAX);
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
        self.copy_target = CopyTarget::CellBody;
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
                body_collapsed: false,
                output_collapsed: false,
            },
        );
        self.selected = next;
        self.copy_target = CopyTarget::CellBody;
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
        self.copy_target = CopyTarget::CellBody;
        self.clear_mouse_text_selection();
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
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            if !self.is_cell_runnable(cell) {
                self.status = format!(
                    "cell is not runnable under kernel={} env={}",
                    self.notebook.metadata.runtime.kernel.display_name(),
                    self.current_environment_label()
                );
                return Ok(());
            }
        }
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
            if self.is_cell_runnable(&self.notebook.cells[index]) {
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

    fn cycle_kernel(&mut self) -> Result<()> {
        self.notebook.metadata.runtime.kernel = match self.notebook.metadata.runtime.kernel {
            KernelKind::Python => KernelKind::Bash,
            KernelKind::Bash => KernelKind::JavaScript,
            KernelKind::JavaScript => KernelKind::Python,
        };
        self.notebook.metadata.kernelspec = self.notebook.metadata.runtime.kernel.kernelspec();
        self.notebook.metadata.language_info = self.notebook.metadata.runtime.kernel.language_info();
        if self.notebook.metadata.runtime.kernel != KernelKind::Python
            && self.notebook.metadata.runtime.environment != "none"
        {
            self.notebook.metadata.runtime.environment = "system".to_string();
        }
        self.reconfigure_runtime()?;
        self.status = format!(
            "kernel set to {}",
            self.notebook.metadata.runtime.kernel.display_name()
        );
        Ok(())
    }

    fn cycle_environment(&mut self) -> Result<()> {
        let options = self.current_environment_options();
        let current = options
            .iter()
            .position(|option| option.id == self.notebook.metadata.runtime.environment)
            .unwrap_or(1.min(options.len().saturating_sub(1)));
        let next = (current + 1) % options.len();
        self.notebook.metadata.runtime.environment = options[next].id.clone();
        self.reconfigure_runtime()?;
        self.status = format!("environment set to {}", options[next].label);
        Ok(())
    }

    fn enter_edit_mode(&mut self) {
        self.mode = AppMode::Edit;
        self.copy_target = CopyTarget::CellBody;
        self.clear_mouse_text_selection();
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            self.cell_modes.entry(cell.id.0.clone()).or_default().body_collapsed = false;
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

    fn toggle_body_for_selected(&mut self) {
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            let mode = self.cell_modes.entry(cell.id.0.clone()).or_default();
            mode.body_collapsed = !mode.body_collapsed;
            if mode.body_collapsed {
                self.clear_mouse_text_selection();
                self.mode = AppMode::Command;
                self.vim = None;
            }
        }
    }

    fn cell_mode(&self, cell: &Cell) -> CellUiState {
        self.cell_modes
            .get(&cell.id.0)
            .cloned()
            .unwrap_or_else(|| CellUiState {
                rendered: cell.kind == CellKind::Markdown,
                body_collapsed: false,
                output_collapsed: false,
            })
    }

    fn activate_python_lsp(&mut self) {
        if self.notebook.metadata.runtime.kernel != KernelKind::Python {
            return;
        }
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
            (AppMode::Command, _) => "command: click cells/buttons | y copy | Y copy block | gy copy output | z fold | o output | e edit | r run | R run all | c code | m markdown | d delete | ctrl-s save | q quit".to_string(),
            (AppMode::Edit, Some(VimMode::Normal)) => {
                "edit vim NORMAL: i insert | y copy in VISUAL | Esc exit editor | alt-z fold | ctrl-c copy | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Insert)) => {
                "edit vim INSERT: Esc normal | alt-z fold | ctrl-c copy | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
            (AppMode::Edit, Some(VimMode::Visual)) => {
                "edit vim VISUAL: y copy to clipboard | d delete | c change | Esc normal".to_string()
            }
            (AppMode::Edit, Some(VimMode::Operator(_))) => {
                "edit vim OPERATOR: motion applies pending operator".to_string()
            }
            (AppMode::Edit, None) => {
                "edit: Esc finish | alt-z fold | ctrl-s save | ctrl-r run | shift-enter run".to_string()
            }
        };
    }

    fn hit_test(&self, column: u16, row: u16) -> Option<HitTarget> {
        self.hit_regions
            .iter()
            .rev()
            .find(|region| contains(region.rect, column, row))
            .map(|region| region.target.clone())
    }

    fn content_region_at(&self, column: u16, row: u16) -> Option<&ContentRegion> {
        self.content_regions
            .iter()
            .rev()
            .find(|region| contains(region.rect, column, row))
    }

    fn mouse_text_selection_for(
        &self,
        cell_index: usize,
        target: CopyTarget,
    ) -> Option<MouseTextSelection> {
        self.mouse_text_selection
            .filter(|selection| selection.cell_index == cell_index && selection.target == target)
    }

    fn clear_mouse_text_selection(&mut self) {
        self.mouse_text_selection = None;
        self.drag_content_selection = false;
    }

    fn activate_hit_target(&mut self, target: HitTarget, column: u16, row: u16) -> Result<()> {
        match target {
            HitTarget::ToolbarSave => self.save_all()?,
            HitTarget::ToolbarRunAll => self.run_all_cells()?,
            HitTarget::ToolbarRestart => self.restart_runtime()?,
            HitTarget::ToolbarCycleKernel => self.cycle_kernel()?,
            HitTarget::ToolbarCycleEnvironment => self.cycle_environment()?,
            HitTarget::ToolbarAddCode => self.insert_code_cell(),
            HitTarget::ToolbarAddMarkdown => self.insert_markdown_cell(),
            HitTarget::CellSelect(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.load_selected_into_editor();
                self.ensure_selected_visible();
            }
            HitTarget::CellEditor(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.enter_edit_mode();
                if let Some(rect) = self.active_editor_rect {
                    self.place_editor_cursor(column, row, rect, false);
                }
            }
            HitTarget::CellOutput(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellOutput;
                self.load_selected_into_editor();
                self.ensure_selected_visible();
            }
            HitTarget::CellRun(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.run_selected_cell()?;
            }
            HitTarget::CellEdit(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.enter_edit_mode();
            }
            HitTarget::CellToggleBody(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.toggle_body_for_selected();
            }
            HitTarget::CellToggleRender(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                if let Some(cell) = self.notebook.cells.get(index) {
                    let mode = self.cell_modes.entry(cell.id.0.clone()).or_default();
                    mode.rendered = !mode.rendered;
                }
            }
            HitTarget::CellToggleOutput(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellOutput;
                self.toggle_output_for_selected();
            }
            HitTarget::CellOpenImage(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellOutput;
                self.open_selected_image()?;
            }
            HitTarget::CellInsertBelow(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                if matches!(self.notebook.cells[index].kind, CellKind::Markdown) {
                    self.insert_markdown_cell();
                } else {
                    self.insert_code_cell();
                }
            }
            HitTarget::CellDelete(index) => {
                self.selected = index;
                self.copy_target = CopyTarget::CellBody;
                self.delete_selected_cell();
            }
        }
        Ok(())
    }

    fn place_editor_cursor(&mut self, column: u16, row: u16, rect: Rect, selecting: bool) {
        let local_row = row
            .saturating_sub(rect.y)
            .saturating_add(self.editor_row_offset as u16);
        let local_col = column.saturating_sub(rect.x);
        if selecting && !self.editor.is_selecting() {
            self.editor.start_selection();
        } else if !selecting {
            self.editor.cancel_selection();
        }
        self.editor.move_cursor(CursorMove::Jump(local_row, local_col));
        self.sync_editor_row_offset(rect.height as usize);
        self.sync_editor_presentation();
    }

    fn register_cell_chrome_hits(&mut self, chrome_area: Rect, index: usize, cell: &Cell, rendered: bool) {
        let mut labels = Vec::new();
        if self.is_cell_runnable(cell) {
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
        labels.push((
            if self.cell_mode(cell).body_collapsed {
                "[Unfold]"
            } else {
                "[Fold]"
            },
            HitTarget::CellToggleBody(index),
        ));
        labels.push(("[Del]", HitTarget::CellDelete(index)));
        if !cell.outputs.is_empty() {
            labels.push((
                if self.cell_mode(cell).output_collapsed {
                    "[Show Out]"
                } else {
                    "[Hide Out]"
                },
                HitTarget::CellToggleOutput(index),
            ));
            if self.first_image_output(cell).is_some() {
                labels.push(("[Open]", HitTarget::CellOpenImage(index)));
            }
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
        if self.cell_mode(cell).body_collapsed {
            return 3;
        }
        let lines = if index == self.selected && self.mode == AppMode::Edit {
            self.editor.lines().len().max(1)
        } else {
            cell.source.lines().count().max(1)
        };
        (lines as u16) + 2
    }

    fn sync_editor_row_offset(&mut self, visible_height: usize) {
        let (cursor_row, _) = self.editor.cursor();
        if visible_height == usize::MAX || visible_height == 0 {
            self.editor_row_offset = 0;
            return;
        }
        if cursor_row < self.editor_row_offset {
            self.editor_row_offset = cursor_row;
            return;
        }
        let last_visible = self.editor_row_offset.saturating_add(visible_height.saturating_sub(1));
        if cursor_row > last_visible {
            self.editor_row_offset = cursor_row.saturating_sub(visible_height.saturating_sub(1));
        }
    }

    fn render_code_editor(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        inner: Rect,
        title: &str,
        cell: &Cell,
    ) {
        let block = Block::default()
            .title(title.to_string())
            .borders(Borders::ALL)
            .border_style(self.theme.style("cell.border.selected"))
            .style(self.theme.style("cell.shell.selected"));
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        self.sync_editor_row_offset(inner.height as usize);
        let selection_style = self.editor.selection_style();
        let lines = self.editor.lines().to_vec();
        let highlighted = SyntaxHighlighter::highlight_with_theme(cell.language, &lines.join("\n"), &self.theme);
        let cursor = self.editor.cursor();
        let selection = self.editor.selection_range();

        let start = self.editor_row_offset;
        let end = usize::min(start + inner.height as usize, lines.len().max(1));
        let mut rendered = Vec::new();
        for line_index in start..end {
            let raw_line = lines.get(line_index).map(String::as_str).unwrap_or("");
            let highlighted_line = highlighted.lines.get(line_index);
            rendered.push(render_editor_line(
                raw_line,
                highlighted_line,
                line_index,
                cursor.0,
                selection,
                self.theme.style("editor.cursor_line"),
                selection_style,
            ));
        }
        if rendered.is_empty() {
            rendered.push(Line::from(String::new()));
        }
        frame.render_widget(Paragraph::new(Text::from(rendered)), inner);

        let cursor_screen_row = cursor.0.saturating_sub(self.editor_row_offset);
        if cursor_screen_row < inner.height as usize {
            let cursor_x = inner.x.saturating_add(cursor.1 as u16);
            let max_x = inner.x.saturating_add(inner.width.saturating_sub(1));
            frame.set_cursor_position(Position::new(
                cursor_x.min(max_x),
                inner.y.saturating_add(cursor_screen_row as u16),
            ));
        }
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

    fn page_scroll_amount(&self) -> isize {
        let mut visible = 0usize;
        let mut used = 0u16;
        let height = self.notebook_area.height.max(1);
        for index in self.scroll_offset as usize..self.notebook.cells.len() {
            let cell_height = self.cell_height(index).saturating_add(1);
            if used.saturating_add(cell_height) > height {
                break;
            }
            used = used.saturating_add(cell_height);
            visible += 1;
        }
        visible.saturating_sub(1).max(1) as isize
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
        let available = self.notebook_area.height;
        let mut used = 0u16;
        let mut new_top = top;
        for index in top..=self.selected {
            let height = self.cell_height(index).saturating_add(1);
            used = used.saturating_add(height);
            while used > available && new_top < self.selected {
                used = used.saturating_sub(self.cell_height(new_top).saturating_add(1));
                new_top += 1;
            }
        }
        self.scroll_offset = new_top as u16;
    }

    fn python_lsp_style(&self) -> Style {
        match self.python_lsp {
            PythonLspStatus::Active { .. } => self.theme.style("lsp.active"),
            PythonLspStatus::Available { .. } => self.theme.style("lsp.available"),
            PythonLspStatus::Unavailable => self.theme.style("lsp.unavailable"),
        }
    }

    fn current_environment_options(&self) -> Vec<EnvironmentOption> {
        discover_environments(
            self.notebook_path.as_deref(),
            self.notebook.metadata.runtime.kernel,
        )
    }

    fn current_environment_label(&self) -> String {
        self.current_environment_options()
            .into_iter()
            .find(|option| option.id == self.notebook.metadata.runtime.environment)
            .map(|option| option.label)
            .unwrap_or_else(|| self.notebook.metadata.runtime.environment.clone())
    }

    fn reconfigure_runtime(&mut self) -> Result<()> {
        self.session
            .configure_for_notebook(&self.notebook, self.notebook_path.as_deref())?;
        self.python_lsp_client = None;
        self.python_lsp = PythonLspStatus::detect();
        self.activate_python_lsp();
        Ok(())
    }

    fn is_cell_runnable(&self, cell: &Cell) -> bool {
        match cell.kind {
            CellKind::Ai => true,
            CellKind::Code => {
                self.notebook.metadata.runtime.environment != "none"
                    && cell.language == self.notebook.metadata.runtime.kernel.language()
            }
            _ => false,
        }
    }

    fn first_image_output<'a>(&self, cell: &'a Cell) -> Option<&'a CellOutput> {
        cell.outputs.iter().find(|output| output.image_info().is_some())
    }

    fn open_selected_image(&mut self) -> Result<()> {
        let Some(cell) = self.notebook.cells.get(self.selected) else {
            return Ok(());
        };
        let Some(output) = self.first_image_output(cell) else {
            self.status = "selected cell has no image output".to_string();
            return Ok(());
        };
        let Some(path) = resolve_image_output_path(output, self.notebook_path.as_deref()) else {
            self.status = "image output has no materialized file path yet".to_string();
            return Ok(());
        };
        open_path_with_system(&path)?;
        self.status = format!("opened image {}", path.display());
        Ok(())
    }

    fn copy_current_target(&mut self) -> Result<()> {
        match self.copy_target {
            CopyTarget::CellBody => self.copy_selected_cell_source(),
            CopyTarget::CellOutput => self.copy_selected_output(),
        }
    }

    fn copy_selected_cell_source(&mut self) -> Result<()> {
        if let Some(text) = self.selected_mouse_text(CopyTarget::CellBody) {
            return self.copy_text("selection", &text);
        }
        let Some(cell) = self.notebook.cells.get(self.selected) else {
            return Ok(());
        };
        let source = cell.source.clone();
        self.copy_text("cell source", &source)
    }

    fn copy_selected_cell_block(&mut self) -> Result<()> {
        let Some(cell) = self.notebook.cells.get(self.selected) else {
            return Ok(());
        };
        let block = match cell.kind {
            CellKind::Markdown => cell.source.clone(),
            CellKind::Code => format!(
                "# cell: code {}\n```{}\n{}\n```",
                cell.language.fence_name(),
                cell.language.fence_name(),
                cell.source
            ),
            CellKind::Raw => format!("# cell: raw\n{}", cell.source),
            CellKind::Ai => format!("# cell: ai\n{}", cell.source),
        };
        self.copy_text("cell block", &block)
    }

    fn copy_selected_output(&mut self) -> Result<()> {
        if let Some(text) = self.selected_mouse_text(CopyTarget::CellOutput) {
            return self.copy_text("selection", &text);
        }
        let Some(cell) = self.notebook.cells.get(self.selected) else {
            return Ok(());
        };
        let text = output_text(cell, self.notebook_path.as_deref());
        if text.is_empty() {
            self.status = "selected cell has no copyable output".to_string();
            return Ok(());
        }
        self.copy_text("cell output", &text)
    }

    fn copy_editor_selection(&mut self) -> Result<()> {
        let Some(text) = selected_editor_text(&self.editor) else {
            self.status = "no editor selection to copy".to_string();
            return Ok(());
        };
        self.copy_text("editor selection", &text)
    }

    fn copy_yank_buffer(&mut self, label: &str) -> Result<()> {
        let text = self.editor.yank_text();
        if text.is_empty() {
            self.status = format!("no {label} to copy");
            return Ok(());
        }
        self.copy_text(label, &text)
    }

    fn copy_text(&mut self, label: &str, text: &str) -> Result<()> {
        match self.clipboard.write_text(text) {
            Ok(result) => {
                self.set_copy_status(label, result);
                Ok(())
            }
            Err(error) => {
                self.status = format!("failed to copy {label}: {error}");
                Ok(())
            }
        }
    }

    fn set_copy_status(&mut self, label: &str, result: ClipboardResult) {
        self.status = format!("copied {label} via {}", result.backend.label());
    }

    fn selected_mouse_text(&self, target: CopyTarget) -> Option<String> {
        let selection = self.mouse_text_selection?;
        if selection.cell_index != self.selected || selection.target != target || selection.is_empty() {
            return None;
        }
        let cell = self.notebook.cells.get(self.selected)?;
        let lines = text_lines_for_target(cell, target, self.notebook_path.as_deref());
        extract_selected_text(&lines, selection)
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

fn text_lines_for_target(
    cell: &Cell,
    target: CopyTarget,
    notebook_path: Option<&std::path::Path>,
) -> Vec<String> {
    match target {
        CopyTarget::CellBody => match cell.kind {
            CellKind::Markdown => text_to_plain_lines(render_markdown_block(&cell.source, &Theme::default_theme())),
            _ => cell.source.lines().map(|line| line.to_string()).collect(),
        },
        CopyTarget::CellOutput => output_text(cell, notebook_path)
            .lines()
            .map(|line| line.to_string())
            .collect(),
    }
}

fn text_to_plain_lines(text: Text<'static>) -> Vec<String> {
    text.lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect()
}

fn apply_mouse_selection_to_text(
    text: Text<'static>,
    selection: Option<MouseTextSelection>,
    selection_style: Style,
) -> Text<'static> {
    let Some(selection) = selection else {
        return text;
    };
    let (start, end) = selection.normalized();
    let mut lines = Vec::with_capacity(text.lines.len());
    for (line_index, line) in text.lines.into_iter().enumerate() {
        lines.push(apply_selection_to_line(
            line,
            line_index,
            start,
            end,
            selection_style,
        ));
    }
    Text::from(lines)
}

fn apply_selection_to_line(
    line: Line<'static>,
    line_index: usize,
    start: TextPoint,
    end: TextPoint,
    selection_style: Style,
) -> Line<'static> {
    let mut chars = flatten_line_spans(line);
    if start.row <= line_index && line_index <= end.row {
        let selection_start = if line_index == start.row { start.col } else { 0 };
        let selection_end = if line_index == end.row {
            end.col
        } else {
            chars.len()
        };
        for (index, (_, style)) in chars.iter_mut().enumerate() {
            if index >= selection_start && index < selection_end {
                *style = style.patch(selection_style);
            }
        }
    }
    merge_styled_chars(chars)
}

fn flatten_line_spans(line: Line<'static>) -> Vec<(String, Style)> {
    let mut chars = Vec::new();
    for span in line.spans {
        for ch in span.content.chars() {
            chars.push((ch.to_string(), span.style));
        }
    }
    if chars.is_empty() {
        vec![(String::new(), Style::default())]
    } else {
        chars
    }
}

fn render_editor_line(
    raw_line: &str,
    highlighted_line: Option<&Line<'static>>,
    line_index: usize,
    cursor_row: usize,
    selection: Option<((usize, usize), (usize, usize))>,
    cursor_line_style: Style,
    selection_style: Style,
) -> Line<'static> {
    let mut chars = flatten_highlighted_line(raw_line, highlighted_line);

    if line_index == cursor_row {
        for (_, style) in &mut chars {
            *style = style.patch(cursor_line_style);
        }
    }

    if let Some(((start_row, start_col), (end_row, end_col))) = selection {
        let selection_start = if line_index == start_row { start_col } else { 0 };
        let selection_end = if line_index == end_row {
            end_col
        } else if start_row <= line_index && line_index < end_row {
            chars.len()
        } else {
            0
        };
        if start_row <= line_index && line_index <= end_row {
            for (index, (_, style)) in chars.iter_mut().enumerate() {
                if index >= selection_start && index < selection_end {
                    *style = style.patch(selection_style);
                }
            }
        }
    }

    merge_styled_chars(chars)
}

fn flatten_highlighted_line(
    raw_line: &str,
    highlighted_line: Option<&Line<'static>>,
) -> Vec<(String, Style)> {
    if let Some(line) = highlighted_line {
        let mut chars = Vec::new();
        for span in &line.spans {
            for ch in span.content.chars() {
                chars.push((ch.to_string(), span.style));
            }
        }
        if !chars.is_empty() {
            return chars;
        }
    }
    raw_line
        .chars()
        .map(|ch| (ch.to_string(), Style::default()))
        .collect()
}

fn merge_styled_chars(chars: Vec<(String, Style)>) -> Line<'static> {
    if chars.is_empty() {
        return Line::from(String::new());
    }
    let mut spans = Vec::new();
    let mut current_style = chars[0].1;
    let mut current_text = String::new();

    for (text, style) in chars {
        if style == current_style {
            current_text.push_str(&text);
        } else {
            spans.push(Span::styled(current_text, current_style));
            current_text = text;
            current_style = style;
        }
    }
    spans.push(Span::styled(current_text, current_style));
    Line::from(spans)
}

fn render_code_block(cell: &Cell, theme: &Theme) -> Text<'static> {
    SyntaxHighlighter::highlight_with_theme(cell.language, &cell.source, theme)
}

fn render_output_block(cell: &Cell, theme: &Theme) -> Text<'static> {
    let mut lines = Vec::new();
    for output in &cell.outputs {
        if let Some(image) = output.image_info() {
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "Image [{}] {}",
                    image.mime,
                    image.alt.unwrap_or_else(|| "open with [Open]".to_string())
                ),
                theme.style("output.stream.label"),
            )]));
            if let Some(path) = image.path {
                lines.push(Line::from(path));
            }
            continue;
        }
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

fn output_text(cell: &Cell, notebook_path: Option<&std::path::Path>) -> String {
    let mut chunks = Vec::new();
    for output in &cell.outputs {
        if let Some(image) = output.image_info() {
            let mut line = format!("Image [{}]", image.mime);
            if let Some(alt) = image.alt {
                line.push_str(&format!(" {alt}"));
            }
            if let Some(path) = image.path {
                let resolved = resolve_image_output_path(output, notebook_path)
                    .unwrap_or_else(|| PathBuf::from(path));
                line.push_str(&format!("\n{}", resolved.display()));
            }
            chunks.push(line);
            continue;
        }
        match output {
            CellOutput::Stream { name, text } => chunks.push(format!("{name}:\n{text}")),
            CellOutput::ExecuteResult {
                execution_count,
                data,
                ..
            } => {
                if let Some(value) = data.get("text/plain").and_then(|value| value.as_str()) {
                    chunks.push(format!("Out [{execution_count}]:\n{value}"));
                }
            }
            CellOutput::DisplayData { data, .. } => {
                if let Some(value) = data.get("text/plain").and_then(|value| value.as_str()) {
                    chunks.push(value.to_string());
                }
            }
            CellOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                let mut text = format!("{ename}: {evalue}");
                if !traceback.is_empty() {
                    text.push('\n');
                    text.push_str(&traceback.join("\n"));
                }
                chunks.push(text);
            }
        }
    }
    chunks.join("\n\n")
}

fn extract_selected_text(lines: &[String], selection: MouseTextSelection) -> Option<String> {
    let (start, end) = selection.normalized();
    if start.row >= lines.len() || end.row >= lines.len() {
        return None;
    }
    let mut selected = String::new();
    for row in start.row..=end.row {
        let line = lines.get(row)?;
        let fragment = if row == start.row && row == end.row {
            line.chars()
                .skip(start.col)
                .take(end.col.saturating_sub(start.col))
                .collect::<String>()
        } else if row == start.row {
            line.chars().skip(start.col).collect::<String>()
        } else if row == end.row {
            line.chars().take(end.col).collect::<String>()
        } else {
            line.clone()
        };
        if !selected.is_empty() {
            selected.push('\n');
        }
        selected.push_str(&fragment);
    }
    Some(selected)
}

fn selected_editor_text(editor: &TextArea<'_>) -> Option<String> {
    let ((start_row, start_col), (end_row, end_col)) = editor.selection_range()?;
    let lines = editor.lines();
    if start_row == end_row {
        let line = lines.get(start_row)?;
        return Some(
            line.chars()
                .skip(start_col)
                .take(end_col.saturating_sub(start_col))
                .collect(),
        );
    }

    let mut selected = String::new();
    for row in start_row..=end_row {
        let line = lines.get(row)?;
        let slice = if row == start_row {
            line.chars().skip(start_col).collect::<String>()
        } else if row == end_row {
            line.chars().take(end_col).collect::<String>()
        } else {
            line.clone()
        };
        if !selected.is_empty() {
            selected.push('\n');
        }
        selected.push_str(&slice);
    }
    Some(selected)
}

fn should_copy_vim_selection(mode: VimMode, input: Input, transition: &VimTransition) -> bool {
    matches!(
        (mode, input.key, transition),
        (VimMode::Visual, Key::Char('y'), VimTransition::Mode(VimMode::Normal))
            | (VimMode::Operator('y'), _, VimTransition::Mode(VimMode::Normal))
    )
}

fn text_point_from_mouse(region: &ContentRegion, column: u16, row: u16, app: &App) -> TextPoint {
    let cell = &app.notebook.cells[region.cell_index];
    let lines = text_lines_for_target(cell, region.target, app.notebook_path.as_deref());
    let local_row = row.saturating_sub(region.rect.y) as usize;
    let line_index = local_row.min(lines.len().saturating_sub(1));
    let line_len = lines.get(line_index).map(|line| line.chars().count()).unwrap_or(0);
    let local_col = column.saturating_sub(region.rect.x) as usize;
    TextPoint {
        row: line_index,
        col: local_col.min(line_len),
    }
}

fn resolve_image_output_path(output: &CellOutput, notebook_path: Option<&std::path::Path>) -> Option<PathBuf> {
    let image = output.image_info()?;
    let path = image.path?;
    let path_buf = PathBuf::from(&path);
    if path_buf.is_absolute() {
        Some(path_buf)
    } else {
        notebook_path
            .and_then(std::path::Path::parent)
            .map(|parent| parent.join(path))
    }
}

fn open_path_with_system(path: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", &path.display().to_string()]);
        command
    };
    #[cfg(not(target_os = "windows"))]
    command.arg(path);
    command
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    Ok(())
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
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
    use crate::clipboard::Clipboard;
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

        app.run_selected_cell().unwrap();

        assert!(app.status.contains("not runnable"));
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
            height: 14,
        };

        app.move_selection(5);

        assert_eq!(app.selected, 5);
        assert!(app.scroll_offset > 0);
        assert!(app.scroll_offset < 5);
    }

    #[test]
    fn editor_line_renderer_preserves_highlighted_content() {
        let theme = Theme::default_theme();
        let highlighted = SyntaxHighlighter::highlight_with_theme(
            Language::Python,
            "def make_blobs(x):",
            &theme,
        );

        let line = render_editor_line(
            "def make_blobs(x):",
            highlighted.lines.first(),
            0,
            0,
            None,
            theme.style("editor.cursor_line"),
            theme.style("cell.prompt.selected"),
        );
        let rendered = format!("{line:?}");

        assert!(rendered.contains("def"));
        assert!(rendered.contains("make_blobs"));
    }

    #[test]
    fn command_mode_y_copies_selected_cell_source() {
        let notebook =
            Notebook::new("Copy").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let (clipboard, memory) = Clipboard::memory();
        let mut app = App::new_with_clipboard(
            notebook,
            None,
            session,
            false,
            Theme::default_theme(),
            None,
            clipboard,
        );

        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(memory.lock().unwrap().last().unwrap(), "print(1)");
        assert!(app.status.contains("copied cell source"));
    }

    #[test]
    fn command_mode_gy_copies_selected_output() {
        let mut cell = Cell::code(Language::Python, "print(1)");
        cell.outputs.push(CellOutput::Stream {
            name: "stdout".to_string(),
            text: "hello".to_string(),
        });
        let notebook = Notebook::new("CopyOut").with_cells(vec![cell]);
        let session = SessionManager::new(&notebook);
        let (clipboard, memory) = Clipboard::memory();
        let mut app = App::new_with_clipboard(
            notebook,
            None,
            session,
            false,
            Theme::default_theme(),
            None,
            clipboard,
        );

        app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(memory.lock().unwrap().last().unwrap(), "stdout:\nhello");
        assert!(app.status.contains("copied cell output"));
    }

    #[test]
    fn control_c_copies_editor_selection() {
        let notebook =
            Notebook::new("CopyEdit").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let (clipboard, memory) = Clipboard::memory();
        let mut app = App::new_with_clipboard(
            notebook,
            None,
            session,
            false,
            Theme::default_theme(),
            None,
            clipboard,
        );

        app.enter_edit_mode();
        app.editor.start_selection();
        app.editor.move_cursor(CursorMove::Forward);
        app.editor.move_cursor(CursorMove::Forward);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();

        assert_eq!(memory.lock().unwrap().last().unwrap(), "pr");
        assert!(app.status.contains("copied editor selection"));
    }

    #[test]
    fn command_mode_y_copies_mouse_selected_output_text() {
        let mut cell = Cell::markdown("# Hello\nworld");
        cell.outputs.push(CellOutput::Stream {
            name: "stdout".to_string(),
            text: "alpha\nbeta".to_string(),
        });
        let notebook = Notebook::new("CopySelection").with_cells(vec![cell]);
        let session = SessionManager::new(&notebook);
        let (clipboard, memory) = Clipboard::memory();
        let mut app = App::new_with_clipboard(
            notebook,
            None,
            session,
            false,
            Theme::default_theme(),
            None,
            clipboard,
        );
        app.copy_target = CopyTarget::CellOutput;
        app.mouse_text_selection = Some(MouseTextSelection {
            cell_index: 0,
            target: CopyTarget::CellOutput,
            anchor: TextPoint { row: 1, col: 1 },
            focus: TextPoint { row: 1, col: 3 },
        });

        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(memory.lock().unwrap().last().unwrap(), "lp");
        assert!(app.status.contains("copied selection"));
    }

    #[test]
    fn extract_selected_text_handles_multi_line_ranges() {
        let lines = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let selection = MouseTextSelection {
            cell_index: 0,
            target: CopyTarget::CellBody,
            anchor: TextPoint { row: 0, col: 2 },
            focus: TextPoint { row: 2, col: 2 },
        };

        let text = extract_selected_text(&lines, selection).unwrap();

        assert_eq!(text, "pha\nbeta\nga");
    }

    #[test]
    fn body_collapse_hides_input_height_and_expands_on_edit() {
        let notebook =
            Notebook::new("Fold").with_cells(vec![Cell::code(Language::Python, "print(1)\nprint(2)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.toggle_body_for_selected();
        let collapsed_height = app.input_height(&app.notebook.cells[0], 0);
        assert_eq!(collapsed_height, 3);

        app.enter_edit_mode();

        assert!(!app.cell_mode(&app.notebook.cells[0]).body_collapsed);
    }

    #[test]
    fn output_toggle_only_affects_output_visibility() {
        let mut cell = Cell::code(Language::Python, "print(1)");
        cell.outputs.push(CellOutput::Stream {
            name: "stdout".to_string(),
            text: "hello".to_string(),
        });
        let notebook = Notebook::new("Output").with_cells(vec![cell]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        let expanded = app.cell_height(0);
        app.toggle_output_for_selected();
        let collapsed = app.cell_height(0);

        assert!(collapsed < expanded);
        assert!(!app.cell_mode(&app.notebook.cells[0]).body_collapsed);
    }

    #[test]
    fn input_height_does_not_clip_long_cell_source() {
        let source = (0..24)
            .map(|index| format!("line_{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let notebook = Notebook::new("Long").with_cells(vec![Cell::code(Language::Python, source)]);
        let session = SessionManager::new(&notebook);
        let app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        assert_eq!(app.input_height(&app.notebook.cells[0], 0), 26);
    }

    #[test]
    fn hit_test_prefers_specific_button_region_over_cell_region() {
        let notebook = Notebook::new("Fold").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);
        app.hit_regions = vec![
            HitRegion {
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 1,
                },
                target: HitTarget::CellSelect(0),
            },
            HitRegion {
                rect: Rect {
                    x: 5,
                    y: 0,
                    width: 6,
                    height: 1,
                },
                target: HitTarget::CellToggleBody(0),
            },
        ];

        let hit = app.hit_test(6, 0);

        assert!(matches!(hit, Some(HitTarget::CellToggleBody(0))));
    }

    #[test]
    fn alt_z_folds_selected_cell_from_edit_mode() {
        let notebook =
            Notebook::new("FoldEdit").with_cells(vec![Cell::code(Language::Python, "print(1)")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.enter_edit_mode();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT))
            .unwrap();

        assert_eq!(app.mode, AppMode::Command);
        assert!(app.cell_mode(&app.notebook.cells[0]).body_collapsed);
    }

    #[test]
    fn clicking_cell_select_only_selects_and_does_not_enter_edit_mode() {
        let notebook = Notebook::new("Click").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.activate_hit_target(HitTarget::CellSelect(0), 0, 0).unwrap();

        assert_eq!(app.mode, AppMode::Command);
    }

    #[test]
    fn clicking_button_target_runs_action_instead_of_selection() {
        let notebook = Notebook::new("Buttons").with_cells(vec![Cell::markdown("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, None, session, false, Theme::default_theme(), None);

        app.activate_hit_target(HitTarget::CellToggleBody(0), 0, 0).unwrap();

        assert!(app.cell_mode(&app.notebook.cells[0]).body_collapsed);
    }
}
