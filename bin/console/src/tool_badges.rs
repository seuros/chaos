use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolAnnotationsView {
    read_only_hint: Option<bool>,
    destructive_hint: Option<bool>,
    idempotent_hint: Option<bool>,
    open_world_hint: Option<bool>,
}

fn parsed_annotations(annotations: Option<&serde_json::Value>) -> Option<ToolAnnotationsView> {
    annotations.and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub(crate) fn tool_name_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn style_for_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Style {
    let mut has_read_only = false;
    let mut has_writes = false;
    let mut has_destructive = false;
    let mut has_idempotent = false;
    let mut has_open_world = false;
    let mut has_closed_world = false;

    for label in labels {
        match label {
            "read-only" => has_read_only = true,
            "writes" => has_writes = true,
            "destructive" => has_destructive = true,
            "idempotent" => has_idempotent = true,
            "open-world" => has_open_world = true,
            "closed-world" => has_closed_world = true,
            _ => {}
        }
    }

    let color = if has_destructive {
        Color::LightRed
    } else if has_writes {
        Color::Yellow
    } else if has_closed_world {
        Color::Blue
    } else if has_open_world {
        Color::Magenta
    } else if has_read_only {
        Color::Cyan
    } else if has_idempotent {
        Color::LightGreen
    } else {
        Color::Cyan
    };

    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub(crate) fn tool_name_style_from_labels(labels: &[String]) -> Style {
    style_for_labels(labels.iter().map(String::as_str))
}

pub(crate) fn tool_name_style_from_annotations(annotations: Option<&serde_json::Value>) -> Style {
    let Some(annotations) = parsed_annotations(annotations) else {
        return tool_name_style();
    };

    let mut labels = Vec::new();
    match annotations.read_only_hint {
        Some(true) => labels.push("read-only"),
        Some(false) => labels.push("writes"),
        None => {}
    }
    if annotations.destructive_hint == Some(true) {
        labels.push("destructive");
    }
    if annotations.idempotent_hint == Some(true) {
        labels.push("idempotent");
    }
    match annotations.open_world_hint {
        Some(true) => labels.push("open-world"),
        Some(false) => labels.push("closed-world"),
        None => {}
    }

    style_for_labels(labels)
}
