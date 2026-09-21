use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::widgets::StatefulWidget;
use ratatui_hypertile::{KeyCode, MouseButton, MouseEventKind};
use serde_json::Value;
use tui_tree_widget::{Tree, TreeItem, TreeState};

pub(super) struct JsonPreview {
    items: Vec<TreeItem<'static, usize>>,
    pub state: TreeState<usize>,
    pub focused: bool,
    pub visible: bool,
    pub area: Rect,
}

impl JsonPreview {
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 128 * 1024 {
            return None;
        }
        let value: Value = serde_json::from_str(text).ok()?;
        if !matches!(value, Value::Object(_) | Value::Array(_)) {
            return None;
        }
        let root = item(&value, "$".into(), 0, &mut 512, 0)?;
        let mut state = TreeState::default();
        state.open(vec![0]);
        state.select(vec![0]);
        Some(Self {
            items: vec![root],
            state,
            focused: false,
            visible: true,
            area: Rect::ZERO,
        })
    }

    pub fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::PageUp => self
                .state
                .select_relative(|i| i.unwrap_or(0).saturating_sub(usize::from(self.area.height))),
            KeyCode::PageDown => self
                .state
                .select_relative(|i| i.unwrap_or(0).saturating_add(usize::from(self.area.height))),
            _ => return super::tree_key(&mut self.state, code),
        };
        true
    }

    pub fn mouse(&mut self, kind: MouseEventKind, position: Position) -> bool {
        if !self.area.contains(position) {
            return false;
        }
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focused = true;
                self.state.click_at(position);
            }
            MouseEventKind::ScrollUp => {
                self.state.key_up();
            }
            MouseEventKind::ScrollDown => {
                self.state.key_down();
            }
            _ => return false,
        }
        true
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool) {
        self.area = area;
        if let Ok(tree) = Tree::new(&self.items) {
            StatefulWidget::render(
                tree.highlight_style(if focused && self.focused {
                    crate::theme::highlight()
                } else {
                    crate::theme::dim()
                }),
                area,
                buf,
                &mut self.state,
            );
        }
    }
}

fn item(
    value: &Value,
    label: String,
    id: usize,
    remaining: &mut usize,
    depth: usize,
) -> Option<TreeItem<'static, usize>> {
    *remaining = remaining.checked_sub(1)?;
    if depth > 32 {
        return None;
    }
    let (text, children) = match value {
        Value::Object(values) => (
            format!("{label}: {{{}}}", values.len()),
            values
                .iter()
                .enumerate()
                .map(|(id, (key, value))| item(value, format!("{key:?}"), id, remaining, depth + 1))
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::Array(values) => (
            format!("{label}: [{}]", values.len()),
            values
                .iter()
                .enumerate()
                .map(|(id, value)| item(value, format!("[{id}]"), id, remaining, depth + 1))
                .collect::<Option<Vec<_>>>()?,
        ),
        value => (format!("{label}: {value}"), Vec::new()),
    };
    TreeItem::new(id, super::single_line(&text, 256), children).ok()
}
