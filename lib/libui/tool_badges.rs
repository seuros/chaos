use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolAnnotationsView {
    #[serde(alias = "readOnlyHint")]
    read_only_hint: Option<bool>,
    #[serde(alias = "destructiveHint")]
    destructive_hint: Option<bool>,
    #[serde(alias = "idempotentHint")]
    idempotent_hint: Option<bool>,
    #[serde(alias = "openWorldHint")]
    open_world_hint: Option<bool>,
}

fn parsed_annotations(annotations: Option<&serde_json::Value>) -> Option<ToolAnnotationsView> {
    annotations.and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub fn tool_name_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
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

pub fn tool_name_style_from_labels(labels: &[String]) -> Style {
    style_for_labels(labels.iter().map(String::as_str))
}

pub fn tool_name_style_from_labels_and_annotations(
    labels: &[String],
    annotations: Option<&serde_json::Value>,
) -> Style {
    let mut merged: Vec<String> = labels.to_vec();

    if let Some(annotations) = parsed_annotations(annotations) {
        let mut push_label = |label: &str| {
            if !merged.iter().any(|existing| existing == label) {
                merged.push(label.to_string());
            }
        };

        match annotations.read_only_hint {
            Some(true) => push_label("read-only"),
            Some(false) => push_label("writes"),
            None => {}
        }
        if annotations.destructive_hint == Some(true) {
            push_label("destructive");
        }
        if annotations.idempotent_hint == Some(true) {
            push_label("idempotent");
        }
        match annotations.open_world_hint {
            Some(true) => push_label("open-world"),
            Some(false) => push_label("closed-world"),
            None => {}
        }
    }

    if merged.is_empty() {
        tool_name_style()
    } else {
        tool_name_style_from_labels(&merged)
    }
}

pub fn tool_name_style_from_annotations(annotations: Option<&serde_json::Value>) -> Style {
    tool_name_style_from_labels_and_annotations(&[], annotations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn merged_annotations_upgrade_closed_world_tool_to_writes() {
        let style = tool_name_style_from_labels_and_annotations(
            &["closed-world".to_string()],
            Some(&serde_json::json!({
                "readOnlyHint": false,
                "openWorldHint": false
            })),
        );

        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn snake_case_annotations_also_render_writes() {
        let style = tool_name_style_from_annotations(Some(&serde_json::json!({
            "read_only_hint": false,
            "open_world_hint": false
        })));

        assert_eq!(style.fg, Some(Color::Yellow));
    }
}
