//! Reflex settings form.
use std::cell::RefCell;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_kern::AuthManager;
use chaos_kern::config::{Config, ReflexBackendSettings};
use chaos_kern::reflex::configuration;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::widgets::{Clear, Paragraph, StatefulWidgetRef, Widget};

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::textarea::{TextArea, TextAreaState};
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;

const NAME: usize = 0;
const URL: usize = 1;
const MODEL: usize = 2;
const PATH: usize = 3;
const TIMEOUT: usize = 4;
const ACCOUNT: usize = 5;
const KEY: usize = 6;
const LABELS: [&str; 7] = [
    "Backend name",
    "Base URL",
    "Model (blank uses the backend default)",
    "Decisions path (Jev only; blank uses default)",
    "Timeout in milliseconds (blank uses default)",
    "Saved provider account (optional ID from /accounts)",
    "New API key (masked; blank keeps the existing credential)",
];

pub struct ReflexSetupForm {
    config: Arc<Config>,
    auth: Arc<AuthManager>,
    process_id: Option<ProcessId>,
    settings: ReflexBackendSettings,
    tx: AppEventSender,
    fields: [TextArea; LABELS.len()],
    states: [RefCell<TextAreaState>; LABELS.len()],
    focused: usize,
    complete: bool,
    error: Option<String>,
}

impl ReflexSetupForm {
    pub fn new(
        config: Arc<Config>,
        auth: Arc<AuthManager>,
        process_id: Option<ProcessId>,
        name: String,
        settings: ReflexBackendSettings,
        tx: AppEventSender,
    ) -> Self {
        let values = [
            name,
            settings.base_url.clone().unwrap_or_default(),
            settings.model.clone().unwrap_or_default(),
            settings.path.clone().unwrap_or_default(),
            settings
                .timeout_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            settings.auth_provider.clone().unwrap_or_default(),
            String::new(),
        ];
        let fields = values.map(|value| {
            let mut field = TextArea::new();
            field.insert_str(&value);
            field
        });
        Self {
            config,
            auth,
            process_id,
            settings,
            tx,
            fields,
            states: std::array::from_fn(|_| RefCell::default()),
            focused: 0,
            complete: false,
            error: None,
        }
    }

    fn value(&self, field: usize) -> Option<String> {
        let value = self.fields[field].text().trim();
        (!value.is_empty()).then(|| value.to_owned())
    }

    fn submit(&mut self) {
        let Some(name) = self.value(NAME) else {
            self.error = Some("Backend name is required".into());
            self.focused = NAME;
            return;
        };
        let mut settings = self.settings.clone();
        settings.base_url = self.value(URL);
        settings.model = self.value(MODEL);
        settings.path = self.value(PATH);
        settings.timeout_ms = match self.value(TIMEOUT).map(|value| value.parse()).transpose() {
            Ok(timeout) => timeout,
            Err(_) => {
                self.error = Some("Timeout must be a positive integer".into());
                self.focused = TIMEOUT;
                return;
            }
        };
        settings.auth_provider = self.value(ACCOUNT);
        if settings.auth_provider.is_some() {
            settings.api_key = None;
            settings.env_key = None;
        }
        if let Err(err) = configuration::validate(&self.config, &settings) {
            self.error = Some(err.to_string());
            return;
        }
        let key = self.value(KEY);
        if key.is_some() && settings.auth_provider.is_some() {
            self.error = Some("Clear the account field to use a new API key".into());
            return;
        }
        let config = self.config.clone();
        let auth = self.auth.clone();
        let tx = self.tx.clone();
        let process_id = self.process_id;
        tokio::spawn(async move {
            let result =
                configuration::save_backend(&config, &auth, &name, settings, key.as_deref())
                    .await
                    .map_err(|err| err.to_string());
            tx.send(AppEvent::ReflexSetupFinished {
                process_id,
                name,
                result,
            });
        });
        self.fields[KEY] = TextArea::new(); // Also discard the field's private kill buffer.
        self.complete = true;
    }
}

impl BottomPaneView for ReflexSetupForm {
    fn handle_key_event(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release {
            return;
        }
        match event.code {
            KeyCode::Esc => {
                self.on_ctrl_c();
            }
            KeyCode::BackTab => self.focused = self.focused.saturating_sub(1),
            KeyCode::Tab => self.focused = (self.focused + 1) % LABELS.len(),
            KeyCode::Enter if event.modifiers == KeyModifiers::NONE => {
                if self.focused == KEY {
                    self.submit();
                } else {
                    self.focused += 1;
                }
            }
            KeyCode::Enter => {}
            KeyCode::Char('j' | 'm') if event.modifiers == KeyModifiers::CONTROL => {}
            _ => {
                self.fields[self.focused].input(event);
                self.error = None;
            }
        }
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.trim().chars().any(char::is_control) {
            self.error = Some("Paste a single-line value".into());
        } else {
            self.fields[self.focused].insert_str(pasted.trim());
            self.error = None;
        }
        true
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.fields[KEY] = TextArea::new();
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

impl Renderable for ReflexSetupForm {
    fn desired_height(&self, _width: u16) -> u16 {
        19
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        Clear.render(area, buf);
        let row = |offset| Rect::new(area.x, area.y + offset, area.width, 1);
        Paragraph::new("Reflex setup · database settings / OS keyring keys".bold())
            .render(row(0), buf);
        let count = usize::from(area.height.saturating_sub(4) / 2).min(LABELS.len());
        let first = (self.focused + 1).saturating_sub(count);
        for (offset, index) in (first..LABELS.len()).take(count).enumerate() {
            let y = 1 + offset as u16 * 2;
            let label = if index == self.focused {
                format!("> {}", LABELS[index]).cyan()
            } else {
                LABELS[index].to_string().dim()
            };
            Paragraph::new(label).render(row(y), buf);
            if index == KEY {
                let mask = if self.fields[KEY].text().is_empty() {
                    ""
                } else {
                    "••••••••"
                };
                Paragraph::new(mask).render(row(y + 1), buf);
            } else {
                (&self.fields[index]).render_ref(
                    row(y + 1),
                    buf,
                    &mut self.states[index].borrow_mut(),
                );
            }
        }
        if area.height >= 3 {
            Paragraph::new("Tab/Shift-Tab move · Enter advances/saves · Esc back".dim())
                .render(row(area.height - 3), buf);
            Paragraph::new(
                "New keys stay local; never sent to chat. Blank key preserves existing auth.".dim(),
            )
            .render(row(area.height - 2), buf);
            if let Some(error) = &self.error {
                Paragraph::new(error.as_str().red()).render(row(area.height - 1), buf);
            }
        }
    }
}

#[cfg(test)]
mod tests;
