//! Read-only inspector state. Network work happens in cancellable background tasks,
//! never in render; resource contents are not submitted to the model.
//! Reads are on demand: selection changes, MCP reloads and `r` request fresh data.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use chaos_ipc::ProcessId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget, Wrap};
use ratatui_hypertile::{EventOutcome, HypertileEvent, KeyCode, MouseButton, MouseEventKind};
use ratatui_hypertile_extras::{HypertilePlugin, PluginContext};
use tui_tree_widget::{Tree, TreeItem, TreeState};

mod images;
mod json;
mod requests;
use images::ImagePreview;
use json::JsonPreview;
use requests::PendingRequest;

#[derive(Clone)]
struct Resource {
    server: String,
    uri: String,
    name: String,
}

#[derive(Default)]
pub(crate) struct InspectorPane {
    process_id: Option<ProcessId>,
    mcp_snapshot: Option<(u64, Vec<String>)>,
    resources: Vec<Resource>,
    // Paths are [server] or [server, URI], never display names or row indices.
    items: Vec<TreeItem<'static, String>>,
    tree: TreeState<String>,
    tree_area: Rect,
    preview_area: Rect,
    scroll: u16,
    content: String,
    image: Option<Box<ImagePreview>>,
    json: Option<JsonPreview>,
    catalog_status: String,
    catalog_loaded: bool,
    content_loaded: bool,
    pending: Option<PendingRequest>,
}

/// The coordinator polls requests; Hypertile owns rendering, input, and unmount.
pub(crate) struct InspectorPlugin(pub Rc<RefCell<InspectorPane>>);

impl HypertilePlugin for InspectorPlugin {
    fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool) {
        self.0.borrow_mut().render(area, buf, focused);
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        self.0.borrow_mut().handle_event(event)
    }

    fn on_unmount(&mut self, _ctx: PluginContext) {
        self.0.borrow_mut().pause();
    }
}

impl InspectorPane {
    pub fn needs_poll(&self) -> bool {
        self.pending.is_some()
            || !self.catalog_loaded
            || (!self.content_loaded && self.selected_resource().is_some())
    }

    pub fn pause(&mut self) {
        // Dropping the request aborts its task AND drops its private reply channel.
        // Even an already-completed reply cannot reach a new selection/process.
        self.pending = None;
        if self.image.take().is_some() {
            // Remounting reads the selected image again, without retaining workers.
            self.content_loaded = false;
        }
        self.tree_area = Rect::ZERO;
        self.preview_area = Rect::ZERO;
        if let Some(json) = &mut self.json {
            json.area = Rect::ZERO;
            json.focused = false;
        }
    }

    pub fn set_process(&mut self, process_id: Option<ProcessId>) {
        if self.process_id != process_id {
            *self = Self {
                process_id,
                ..Self::default()
            };
        }
    }

    fn refresh_catalog(&mut self) {
        self.pause();
        self.catalog_loaded = false;
        self.catalog_status.clear();
        self.reset_preview();
    }

    fn reset_preview(&mut self) {
        self.content.clear();
        self.image = None;
        self.json = None;
        self.content_loaded = false;
        self.scroll = 0;
    }

    fn sync_mcp_servers(&mut self, revision: u64, servers: &[String]) {
        if self
            .mcp_snapshot
            .as_ref()
            .is_some_and(|(old_revision, old_servers)| {
                *old_revision == revision && old_servers == servers
            })
        {
            return;
        }
        // Cancel old-generation replies before polling them, but keep tree identity
        // and expansion state so the refreshed catalog can retain live selections.
        self.refresh_catalog();
        self.mcp_snapshot = Some((revision, servers.to_vec()));
    }

    fn selected_resource(&self) -> Option<&Resource> {
        let [server, uri] = self.tree.selected() else {
            return None;
        };
        self.resources
            .iter()
            .find(|resource| &resource.server == server && &resource.uri == uri)
    }

    fn handle_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        if let Some(json) = &mut self.json {
            match *event {
                HypertileEvent::Key(key) if key.modifiers.is_empty() => {
                    if key.code == KeyCode::Char('j') {
                        json.visible = !json.visible;
                        json.focused = false;
                        json.area = Rect::ZERO;
                        self.scroll = 0;
                        return EventOutcome::Consumed;
                    }
                    if json.visible && key.code == KeyCode::Enter {
                        json.focused = !json.focused;
                        return EventOutcome::Consumed;
                    }
                    if json.visible && json.focused && json.key(key.code) {
                        return EventOutcome::Consumed;
                    }
                }
                HypertileEvent::Mouse(mouse) if mouse.modifiers.is_empty() => {
                    let position = Position::new(mouse.column, mouse.row);
                    if json.visible && json.mouse(mouse.kind, position) {
                        return EventOutcome::Consumed;
                    }
                    if self.tree_area.contains(position)
                        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    {
                        json.focused = false;
                    }
                }
                _ => {}
            }
        }
        let previous = self.tree.selected().to_vec();
        match *event {
            HypertileEvent::Key(key) if key.modifiers.is_empty() => match key.code {
                // Resource leaves must not acquire expansion state.
                KeyCode::Right if self.tree.selected().len() != 1 => {}
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_sub(self.preview_area.height.max(1));
                }
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(self.preview_area.height.max(1));
                }
                KeyCode::Char('r') if self.catalog_loaded => {
                    self.refresh_catalog();
                }
                // Holding the refresh key must not continuously cancel the catalog read.
                KeyCode::Char('r') => {}
                code => {
                    if !tree_key(&mut self.tree, code) {
                        return EventOutcome::Ignored;
                    }
                }
            },
            HypertileEvent::Mouse(mouse) if mouse.modifiers.is_empty() => {
                let position = Position::new(mouse.column, mouse.row);
                if self.tree_area.contains(position) {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Toggle only server groups, not resource leaves.
                            if let Some(path) =
                                self.tree.rendered_at(position).map(<[String]>::to_vec)
                            {
                                if path.len() == 1 && self.tree.selected() == path {
                                    self.tree.toggle_selected();
                                } else {
                                    self.tree.select(path);
                                }
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            self.tree.key_up();
                        }
                        MouseEventKind::ScrollDown => {
                            self.tree.key_down();
                        }
                        _ => return EventOutcome::Ignored,
                    }
                } else if self.preview_area.contains(position) {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
                        MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(1),
                        _ => return EventOutcome::Ignored,
                    }
                } else {
                    return EventOutcome::Ignored;
                }
            }
            _ => return EventOutcome::Ignored,
        }
        if self.tree.selected() != previous {
            // A catalog refresh can keep running while navigating its old tree.
            // A content request must never outlive its selected leaf.
            if self.catalog_loaded {
                self.pending = None;
            }
            self.reset_preview();
        }
        EventOutcome::Consumed
    }

    fn set_catalog(&mut self, mut resources: Vec<Resource>, status: String) -> std::io::Result<()> {
        resources.sort_by(|a, b| (&a.server, &a.uri).cmp(&(&b.server, &b.uri)));
        resources.dedup_by(|a, b| a.server == b.server && a.uri == b.uri);
        let mut groups = BTreeMap::<String, Vec<TreeItem<'static, String>>>::new();
        for resource in &resources {
            groups
                .entry(resource.server.clone())
                .or_default()
                .push(TreeItem::new_leaf(
                    resource.uri.clone(),
                    single_line(&resource.name, 128),
                ));
        }
        let items = groups
            .into_iter()
            .map(|(server, children)| {
                TreeItem::new(server.clone(), single_line(&server, 128), children)
            })
            .collect::<std::io::Result<Vec<_>>>()?;

        // Rebuild the widget's cached geometry, retaining only live identities.
        let mut tree = TreeState::default();
        for item in &items {
            let path = vec![item.identifier().clone()];
            if self.tree.opened().contains(&path)
                || !self
                    .items
                    .iter()
                    .any(|old| old.identifier() == item.identifier())
            {
                tree.open(path);
            }
        }
        let selected = self.tree.selected().to_vec();
        let exists = match selected.as_slice() {
            [server] => items.iter().any(|item| item.identifier() == server),
            [server, uri] => resources
                .iter()
                .any(|r| &r.server == server && &r.uri == uri),
            _ => false,
        };
        if exists {
            tree.select(selected);
        } else if let Some(first) = items.first() {
            let mut path = vec![first.identifier().clone()];
            if tree.opened().contains(&path)
                && let Some(child) = first.child(0)
            {
                path.push(child.identifier().clone());
            }
            tree.select(path);
        }
        self.resources = resources;
        self.items = items;
        self.tree = tree;
        self.catalog_status = status;
        self.catalog_loaded = true;
        self.reset_preview();
        self.tree_area = Rect::ZERO;
        self.preview_area = Rect::ZERO;
        Ok(())
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool) {
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
        [self.tree_area, self.preview_area] = Layout::vertical([
            Constraint::Length((inner.height / 3).min(8)),
            Constraint::Fill(1),
        ])
        .areas(inner);
        if let Ok(tree) = Tree::new(&self.items) {
            StatefulWidget::render(
                tree.highlight_style(crate::theme::highlight())
                    .highlight_symbol("> "),
                self.tree_area,
                buf,
                &mut self.tree,
            );
        }
        if self.preview_area.is_empty() {
            if let Some(json) = &mut self.json {
                json.area = Rect::ZERO;
            }
            return;
        }
        if self.json.as_ref().is_some_and(|json| json.visible) {
            let uri = self
                .selected_resource()
                .map(|resource| single_line(&resource.uri, 512))
                .unwrap_or_default();
            let mut lines = vec![
                Line::from("Enter JSON/resources · j text · r reload"),
                Line::from(uri),
            ];
            if !self.catalog_status.is_empty() {
                lines.push(Line::from(single_line(&self.catalog_status, 512)));
            }
            let [header, area] =
                Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Fill(1)])
                    .areas(self.preview_area);
            Paragraph::new(lines).render(header, buf);
            if let Some(json) = &mut self.json {
                json.render(area, buf, focused);
            }
            return;
        }
        if let Some(image) = &mut self.image {
            let [text_area, image_area] = Layout::vertical([
                Constraint::Length((self.preview_area.height / 3).max(3)),
                Constraint::Fill(1),
            ])
            .areas(self.preview_area);
            self.preview_area = text_area;
            image.render(image_area, buf);
        }
        let mut lines = vec![
            Line::from("↑↓ select · ←→ fold · r reload"),
            Line::from(if self.json.is_some() {
                "PgUp/PgDn preview · j JSON"
            } else {
                "PgUp/PgDn preview"
            }),
        ];
        if let Some(resource) = self.selected_resource() {
            lines.push(Line::from(single_line(&resource.uri, 512)));
            lines.push(Line::from(""));
            lines.extend(self.content.lines().map(|line| Line::from(line.to_owned())));
        } else if self.resources.is_empty() && self.catalog_loaded {
            lines.push(Line::from("No listed resources. Press r to reload."));
        } else {
            lines.push(Line::from("Select a resource to preview."));
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
            .line_count(self.preview_area.width)
            .saturating_sub(usize::from(self.preview_area.height));
        self.scroll = self.scroll.min(max_scroll.min(u16::MAX as usize) as u16);
        paragraph
            .scroll((self.scroll, 0))
            .render(self.preview_area, buf);
    }
}

/// Shared tree movement; callers own paging and leaf-expansion policy.
fn tree_key<Id: Clone + Eq + std::hash::Hash>(tree: &mut TreeState<Id>, code: KeyCode) -> bool {
    match code {
        KeyCode::Up => tree.key_up(),
        KeyCode::Down => tree.key_down(),
        KeyCode::Left => tree.key_left(),
        KeyCode::Right => tree.key_right(),
        KeyCode::Home => tree.select_first(),
        KeyCode::End => tree.select_last(),
        _ => return false,
    };
    true
}

/// Treat resource text as data, not terminal escape sequences.
fn plain_text(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(limit)
        .collect()
}

fn single_line(text: &str, limit: usize) -> String {
    plain_text(text, limit).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod tests;
