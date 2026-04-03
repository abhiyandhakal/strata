use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};

use crate::core::Notebook;

pub struct App {
    pub notebook: Notebook,
    pub selected: usize,
    pub status: String,
}

impl App {
    pub fn new(notebook: Notebook) -> Self {
        Self {
            notebook,
            selected: 0,
            status: "q quit | j/k move".to_string(),
        }
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

    fn event_loop<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.selected = usize::min(
                                self.selected + 1,
                                self.notebook.cells.len().saturating_sub(1),
                            );
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.selected = self.selected.saturating_sub(1);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(3)])
            .split(frame.area());
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(chunks[0]);

        let items: Vec<ListItem<'_>> = self
            .notebook
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let marker = if index == self.selected { ">" } else { " " };
                let content = format!("{marker} {} {}", cell.language.fence_name(), cell.id.0);
                ListItem::new(Line::from(content))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(self.notebook.metadata.title.as_str())
                .borders(Borders::ALL),
        );
        frame.render_widget(list, body[0]);

        let detail = self
            .notebook
            .cells
            .get(self.selected)
            .map(|cell| {
                Paragraph::new(Text::from(cell.source.clone())).block(
                    Block::default()
                        .title(format!("{:?} {:?}", cell.kind, cell.language))
                        .borders(Borders::ALL),
                )
            })
            .unwrap_or_else(|| {
                Paragraph::new("No cells").block(Block::default().borders(Borders::ALL))
            });
        frame.render_widget(detail, body[1]);

        let status = Paragraph::new(self.status.as_str()).block(
            Block::default().borders(Borders::ALL).title(Line::styled(
                "Status",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        );
        frame.render_widget(status, chunks[1]);
    }
}

pub fn should_launch_tui() -> bool {
    io::stdout().is_terminal() && io::stdin().is_terminal() && terminal::size().is_ok()
}
