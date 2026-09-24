//! Storage bootstrap runs before the configuration-dependent onboarding steps.
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use chaos_ipc::product::OS_NAME;
use chaos_kern::user_settings::storage_setup::{self, StorageChoice};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use tokio_stream::StreamExt;

use crate::render::renderable::{ColumnRenderable, Renderable};
use crate::selection_list::selection_option_row;
use crate::tui::{self, Tui, TuiEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Postgres,
    Sqlite,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Choose,
    Postgres,
    Connecting,
}

struct StorageScreen {
    highlighted: Backend,
    page: Page,
    connection: String,
    error: Option<String>,
    should_exit: bool,
}

impl Default for StorageScreen {
    fn default() -> Self {
        Self {
            highlighted: Backend::Postgres,
            page: Page::Choose,
            connection: String::new(),
            error: None,
            should_exit: false,
        }
    }
}

impl StorageScreen {
    fn handle_key_event(&mut self, key: KeyEvent) -> Option<StorageChoice> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'd'))
        {
            self.should_exit = true;
            return None;
        }
        if key.code == KeyCode::Esc {
            if self.page == Page::Choose {
                self.should_exit = true;
            } else {
                self.page = Page::Choose;
                self.error = None;
            }
            return None;
        }
        match self.page {
            Page::Choose => match key.code {
                KeyCode::Up | KeyCode::Char('k' | '1') => {
                    self.highlighted = Backend::Postgres;
                    self.error = None;
                }
                KeyCode::Down | KeyCode::Char('j' | '2') => {
                    self.highlighted = Backend::Sqlite;
                    self.error = None;
                }
                KeyCode::Enter => {
                    self.error = None;
                    match self.highlighted {
                        Backend::Postgres => self.page = Page::Postgres,
                        Backend::Sqlite => {
                            self.page = Page::Connecting;
                            return Some(StorageChoice::Sqlite);
                        }
                    }
                }
                KeyCode::Char('q') => self.should_exit = true,
                _ => {}
            },
            Page::Postgres => match key.code {
                KeyCode::Enter => {
                    if self.connection.trim().is_empty() {
                        self.error =
                            Some("Enter a PostgreSQL URL or env:VARIABLE reference.".into());
                    } else {
                        self.error = None;
                        self.page = Page::Connecting;
                        return Some(StorageChoice::Postgres(self.connection.clone()));
                    }
                }
                KeyCode::Backspace => {
                    self.connection.pop();
                    self.error = None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.connection.clear();
                    self.error = None;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    self.connection.push(c);
                    self.error = None;
                }
                _ => {}
            },
            Page::Connecting => {}
        }
        None
    }

    fn handle_paste(&mut self, text: String) {
        if self.page == Page::Postgres {
            self.connection
                .extend(text.trim().chars().filter(|c| !c.is_control()));
            self.error = None;
        }
    }

    fn failed(&mut self, error: anyhow::Error) {
        self.page = match self.highlighted {
            Backend::Postgres => Page::Postgres,
            Backend::Sqlite => Page::Choose,
        };
        self.error = Some(error.to_string());
    }
}

impl Widget for &StorageScreen {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let mut column = ColumnRenderable::new();
        column.push(Line::from(format!("Welcome to {OS_NAME}")).bold());
        column.push("");
        column.push(Line::from(format!("Where should {OS_NAME} store your data?")).bold());
        column.push(
            Paragraph::new("Your database holds settings, sessions, and remembered approvals.")
                .wrap(Wrap { trim: false }),
        );
        column.push("");
        match self.page {
            Page::Choose => {
                column.push(selection_option_row(
                    0,
                    "PostgreSQL (Recommended)".into(),
                    self.highlighted == Backend::Postgres,
                ));
                column.push(
                    Paragraph::new(
                        "     Connect to a PostgreSQL server you manage, local or remote.",
                    )
                    .wrap(Wrap { trim: false }),
                );
                column.push("");
                column.push(selection_option_row(
                    1,
                    "SQLite".into(),
                    self.highlighted == Backend::Sqlite,
                ));
                column.push(
                    Paragraph::new("     Store data on this machine. No database server required.")
                        .wrap(Wrap { trim: false }),
                );
                column.push("");
                column
                    .push(Line::from("Up/Down to choose · Enter to continue · Esc to quit").dim());
            }
            Page::Postgres => {
                column.push(Line::from("PostgreSQL connection").bold());
                column.push(
                    Paragraph::new(format!(
                        "Use an existing database owned by you. {OS_NAME} will initialize its tables.",
                    ))
                    .wrap(Wrap { trim: false }),
                );
                column.push("Example: postgresql://user:password@localhost:5432/chaos");
                column.push("Or: env:CHAOS_DATABASE_URL");
                column.push("");
                column.push(
                    Paragraph::new(format!("> {}_", self.connection)).wrap(Wrap { trim: false }),
                );
                column.push("");
                #[cfg(target_os = "freebsd")]
                let storage_notice = "URLs use the credential store, or plaintext config.toml.";
                #[cfg(not(target_os = "freebsd"))]
                let storage_notice = "URLs use the secure credential store.";
                column.push(Paragraph::new(storage_notice).wrap(Wrap { trim: false }));
                column.push("");
                column
                    .push(Line::from("Enter to connect · Ctrl+U to clear · Esc to go back").dim());
            }
            Page::Connecting => {
                column.push("Connecting and initializing the database...");
                column.push(Line::from("Esc to cancel · Ctrl+C to quit").dim());
            }
        }
        if let Some(error) = &self.error {
            column.push("");
            column.push(
                Paragraph::new(error.as_str())
                    .red()
                    .wrap(Wrap { trim: false }),
            );
        }
        column.render(area, buf);
    }
}

/// Returns false on cancellation. Always restore terminal modes, including on
/// errors, before ordinary configuration loading and the main TUI take over.
pub(crate) async fn run(home: &Path) -> std::io::Result<bool> {
    let terminal = tui::init()?;
    let mut tui = Tui::new(terminal);
    let result = run_screen(home, &mut tui).await;
    let _ = tui.terminal.clear();
    let restored = tui::restore();
    result.and_then(|completed| restored.map(|()| completed))
}

async fn run_screen(home: &Path, tui: &mut Tui) -> std::io::Result<bool> {
    let mut screen = StorageScreen::default();
    let mut events = tui.event_stream();
    let mut setup: Option<Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>> = None;
    loop {
        tui.draw(u16::MAX, |frame| {
            frame.render_widget(&screen, frame.area());
        })?;
        tokio::select! {
            result = async {
                match setup.as_mut() {
                    Some(future) => future.await,
                    None => std::future::pending().await,
                }
            } => {
                setup = None;
                match result {
                    Ok(()) => return Ok(true),
                    Err(error) => screen.failed(error),
                }
            }
            event = events.next() => {
                match event {
                    Some(TuiEvent::Key(key)) => {
                        if let Some(choice) = screen.handle_key_event(key) {
                            setup = Some(Box::pin(storage_setup::configure(home, choice)));
                        }
                        if screen.should_exit {
                            return Ok(false);
                        }
                        if screen.page != Page::Connecting {
                            setup = None;
                        }
                    }
                    Some(TuiEvent::Paste(text)) => screen.handle_paste(text),
                    Some(TuiEvent::Draw | TuiEvent::Mouse(_)) => {}
                    None => return Ok(false),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
