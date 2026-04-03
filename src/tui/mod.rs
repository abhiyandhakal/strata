use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tui_textarea::{Input, Key, TextArea};

use crate::core::{Cell, CellKind, ExecutionStatus, Language, Notebook};
use crate::runtime::SessionManager;
use crate::storage::{CheckpointPaths, CheckpointStorage, NotebookStorage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Normal,
    Edit,
}

pub struct App {
    pub notebook: Notebook,
    pub selected: usize,
    pub status: String,
    notebook_path: Option<PathBuf>,
    checkpoint_paths: Option<CheckpointPaths>,
    pub session: SessionManager,
    mode: Mode,
    editor: TextArea<'static>,
}

impl App {
    pub fn new(
        notebook: Notebook,
        notebook_path: Option<PathBuf>,
        session: SessionManager,
    ) -> Self {
        let checkpoint_paths = notebook_path
            .as_ref()
            .map(|path| CheckpointPaths::for_notebook(path));
        let mut app = Self {
            notebook,
            selected: 0,
            status:
                "normal: j/k move | e edit | r run | ctrl-s save | b/p/j/t/a add | x delete | q quit"
                    .to_string(),
            notebook_path,
            checkpoint_paths,
            session,
            mode: Mode::Normal,
            editor: TextArea::default(),
        };
        app.load_selected_into_editor();
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
            Mode::Normal => self.handle_normal_mode(key),
            Mode::Edit => self.handle_edit_mode(key),
        }
    }

    fn handle_normal_mode(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('e') => {
                self.mode = Mode::Edit;
                self.status = "edit: Esc finish | ctrl-s save | ctrl-r run".to_string();
            }
            KeyCode::Char('r') => self.run_selected_cell()?,
            KeyCode::Char('b') => self.insert_cell(Language::Bash),
            KeyCode::Char('p') => self.insert_cell(Language::Python),
            KeyCode::Char('J') => self.insert_cell(Language::JavaScript),
            KeyCode::Char('t') => self.insert_cell(Language::TypeScript),
            KeyCode::Char('a') => self.insert_ai_cell(),
            KeyCode::Char('n') => self.insert_text_cell(),
            KeyCode::Char('x') => self.delete_selected_cell(),
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
        if key.code == KeyCode::Esc {
            self.apply_editor_to_cell();
            self.mode = Mode::Normal;
            self.status = "normal: j/k move | e edit | r run | ctrl-s save | b/p/j/t/a add | x delete | q quit".to_string();
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

        self.editor.set_block(
            Block::default()
                .title(format!(
                    "{} {:?}",
                    if self.mode == Mode::Edit {
                        "Editing"
                    } else {
                        "Cell"
                    },
                    self.notebook
                        .cells
                        .get(self.selected)
                        .map(|cell| cell.language)
                        .unwrap_or(Language::Text)
                ))
                .borders(Borders::ALL),
        );
        frame.render_widget(&self.editor, body[1]);

        let output = self.render_output();
        frame.render_widget(output, body[2]);

        let status = Paragraph::new(self.status.as_str())
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(Line::styled(
                format!("Status [{:?}]", self.mode),
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
    }

    fn load_selected_into_editor(&mut self) {
        self.editor = TextArea::default();
        if let Some(cell) = self.notebook.cells.get(self.selected) {
            self.editor = TextArea::from(cell.source.lines().map(|line| line.to_string()));
        }
        self.editor
            .set_cursor_line_style(Style::default().add_modifier(Modifier::REVERSED));
    }

    fn apply_editor_to_cell(&mut self) {
        if let Some(cell) = self.notebook.cells.get_mut(self.selected) {
            cell.source = self.editor.lines().join("\n");
        }
    }

    fn insert_cell(&mut self, language: Language) {
        let next = self.selected.saturating_add(1);
        self.notebook
            .cells
            .insert(next, Cell::code(language, String::new()));
        self.selected = next;
        self.mode = Mode::Edit;
        self.load_selected_into_editor();
        self.status = format!("inserted {} cell", language.fence_name());
    }

    fn insert_ai_cell(&mut self) {
        let next = self.selected.saturating_add(1);
        self.notebook.cells.insert(next, Cell::ai(String::new()));
        self.selected = next;
        self.mode = Mode::Edit;
        self.load_selected_into_editor();
        self.status = "inserted ai cell".to_string();
    }

    fn insert_text_cell(&mut self) {
        let next = self.selected.saturating_add(1);
        self.notebook.cells.insert(next, Cell::text(String::new()));
        self.selected = next;
        self.mode = Mode::Edit;
        self.load_selected_into_editor();
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
            self.mode = Mode::Normal;
        }
        Ok(())
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
        let mut app = App::new(notebook, None, session);

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
    }

    #[test]
    fn app_save_writes_notebook_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("demo.md");
        let notebook = Notebook::new("Save").with_cells(vec![Cell::text("hello")]);
        let session = SessionManager::new(&notebook);
        let mut app = App::new(notebook, Some(path.clone()), session);

        app.save_all().unwrap();

        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("# Save"));
    }
}
