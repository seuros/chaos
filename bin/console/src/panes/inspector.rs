//! Read-only inspector state. Network work happens in cancellable background tasks,
//! never in render; resource contents are not submitted to the model.
//! Reads are on demand, not periodic: selection changes and `r` request fresh data.

use std::sync::Arc;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_kern::Process;
use chaos_mcp_runtime::ResourceContents;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use tokio::task::JoinHandle;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESOURCES: usize = 128;
const MAX_CONTENT_CHARS: usize = 32_768;

#[derive(Clone)]
struct Resource {
    server: String,
    uri: String,
    name: String,
}

enum Update {
    Catalog(Vec<Resource>, String),
    Content(String),
}

pub(crate) struct InspectorPane {
    process_id: Option<ProcessId>,
    resources: Vec<Resource>,
    selected: usize,
    scroll: u16,
    content: String,
    catalog_status: String,
    catalog_loaded: bool,
    content_loaded: bool,
    pending: Option<JoinHandle<Update>>,
}

impl Default for InspectorPane {
    fn default() -> Self {
        Self {
            process_id: None,
            resources: Vec::new(),
            selected: 0,
            scroll: 0,
            content: String::new(),
            catalog_status: String::new(),
            catalog_loaded: false,
            content_loaded: false,
            pending: None,
        }
    }
}

impl Drop for InspectorPane {
    fn drop(&mut self) {
        self.pause();
    }
}

impl InspectorPane {
    pub fn needs_poll(&self) -> bool {
        self.pending.is_some()
            || !self.catalog_loaded
            || (!self.content_loaded && !self.resources.is_empty())
    }

    pub fn pause(&mut self) {
        if let Some(task) = self.pending.take() {
            task.abort();
        }
    }

    pub fn set_process(&mut self, process_id: Option<ProcessId>) {
        if self.process_id != process_id {
            self.pause();
            self.process_id = process_id;
            self.resources.clear();
            self.catalog_loaded = false;
            self.catalog_status.clear();
            self.content.clear();
            self.selected = 0;
            self.scroll = 0;
            self.content_loaded = false;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        match key.code {
            KeyCode::Left | KeyCode::Right if !self.resources.is_empty() => {
                let count = self.resources.len();
                self.selected = if key.code == KeyCode::Right {
                    (self.selected + 1) % count
                } else {
                    (self.selected + count - 1) % count
                };
                self.pause();
                self.content.clear();
                self.scroll = 0;
                self.content_loaded = false;
            }
            KeyCode::Char('r') if key.kind == KeyEventKind::Press => {
                self.pause();
                self.catalog_loaded = false;
                self.content_loaded = false;
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = u16::MAX,
            _ => {}
        }
    }

    fn finish_request(&mut self, result: Result<Update, tokio::task::JoinError>) {
        match result {
            Ok(Update::Catalog(resources, status)) => {
                let previous = self.resources.get(self.selected);
                let selected = previous.and_then(|old| {
                    resources
                        .iter()
                        .position(|new| old.server == new.server && old.uri == new.uri)
                });
                self.resources = resources;
                self.selected = selected.unwrap_or(0);
                self.catalog_status = status;
                self.catalog_loaded = true;
                self.content.clear();
                self.content_loaded = false;
            }
            Ok(Update::Content(text)) => {
                self.content = text;
                self.content_loaded = true;
            }
            Err(err) => {
                self.content = format!("Resource worker failed: {err}");
                // A failed worker must not turn redraws into automatic retries.
                self.catalog_loaded = true;
                self.content_loaded = true;
            }
        }
    }

    /// Called from the event loop, before rendering. No request blocks the UI.
    pub async fn tick(&mut self, process: Arc<Process>, servers: Vec<String>) {
        if self.pending.as_ref().is_some_and(|task| task.is_finished())
            && let Some(task) = self.pending.take()
        {
            self.finish_request(task.await);
        }
        if self.pending.is_some() || !self.needs_poll() {
            return;
        }
        if !self.catalog_loaded {
            self.pending = Some(tokio::spawn(async move {
                let mut resources = Vec::new();
                let mut errors = Vec::new();
                for server in servers {
                    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
                        let mut cursor = None;
                        // Bound pagination even if a buggy server repeats its cursor.
                        for _ in 0..8 {
                            let page = process.list_mcp_resources(&server, cursor).await?;
                            for item in page.resources {
                                if resources.len() == MAX_RESOURCES {
                                    break;
                                }
                                if item.uri.len() <= 4096 {
                                    resources.push(Resource {
                                        server: server.clone(),
                                        uri: item.uri,
                                        name: plain_text(&item.name, 128),
                                    });
                                }
                            }
                            cursor = page.next_cursor;
                            if cursor.is_none() || resources.len() == MAX_RESOURCES {
                                break;
                            }
                        }
                        anyhow::Ok(())
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => errors.push(format!("{server}: {err}")),
                        Err(_) => errors.push(format!("{server}: timed out")),
                    }
                    if resources.len() == MAX_RESOURCES {
                        errors.push("Resource list limited to 128 entries.".into());
                        break;
                    }
                }
                Update::Catalog(resources, plain_text(&errors.join("\n"), 2048))
            }));
        } else if let Some(resource) = self.resources.get(self.selected).cloned() {
            self.pending = Some(tokio::spawn(async move {
                let result = tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    process.read_mcp_resource(&resource.server, resource.uri),
                )
                .await;
                let text = match result {
                    Ok(Ok(result)) => {
                        let mut text = String::new();
                        let mut remaining = MAX_CONTENT_CHARS;
                        for content in result.contents {
                            let value = match content {
                                ResourceContents::Text(value) => value.text,
                                ResourceContents::Blob(_) => "[Binary resource omitted]".into(),
                            };
                            let bounded = plain_text(&value, remaining);
                            remaining = remaining.saturating_sub(bounded.chars().count() + 1);
                            text.push_str(&bounded);
                            text.push('\n');
                            if remaining == 0 {
                                text.push_str("[Content truncated]");
                                break;
                            }
                        }
                        text
                    }
                    Ok(Err(err)) => plain_text(&format!("Read failed: {err}"), 2048),
                    Err(_) => "Resource read timed out. Press r to retry.".into(),
                };
                Update::Content(text)
            }));
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" MCP resources ")
            .title_bottom(" Esc/Tab chat · F4 hide ")
            .border_style(if focused {
                crate::theme::highlight()
            } else {
                crate::theme::border()
            });
        let inner = block.inner(area);
        block.render(area, buf);
        let mut lines = vec![Line::from("←/→ resource · r reload"), Line::from("")];
        if let Some(resource) = self.resources.get(self.selected) {
            lines.push(Line::from(format!(
                "{} / {}",
                plain_text(&resource.server, 128),
                resource.name
            )));
            lines.push(Line::from(plain_text(&resource.uri, 512)));
            lines.push(Line::from(""));
            lines.extend(self.content.lines().map(|line| Line::from(line.to_owned())));
        } else if self.catalog_loaded {
            lines.push(Line::from("No listed resources. Press r to reload."));
        }
        if self.pending.is_some() {
            lines.push(Line::from("Refreshing…"));
        }
        lines.extend(
            self.catalog_status
                .lines()
                .map(|line| Line::from(line.to_owned())),
        );
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let max_scroll = paragraph
            .line_count(inner.width)
            .saturating_sub(usize::from(inner.height));
        paragraph
            .scroll((self.scroll.min(max_scroll.min(u16::MAX as usize) as u16), 0))
            .render(inner, buf);
    }
}

/// Treat resource text as data, not terminal escape sequences.
fn plain_text(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn inspector_resource_navigation_does_not_retain_another_sessions_content() {
        let mut pane = InspectorPane::default();
        pane.set_process(Some(ProcessId::new()));
        pane.resources = vec![
            Resource {
                server: "mcp".into(),
                uri: "state://one".into(),
                name: "one".into(),
            },
            Resource {
                server: "mcp".into(),
                uri: "state://two".into(),
                name: "two".into(),
            },
        ];
        pane.content = "old resource".into();
        pane.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(pane.selected, 1);
        assert!(pane.content.is_empty());

        pane.content = "private session data".into();
        pane.set_process(Some(ProcessId::new()));
        assert!(pane.resources.is_empty());
        assert!(pane.content.is_empty());
        assert!(!pane.catalog_loaded);
        let text = plain_text("\u{1b}[2Jhello\u{7}\nworld", 9);
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{7}'));
        assert_eq!(text.chars().count(), 9);
    }
}
