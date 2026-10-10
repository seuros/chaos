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
    annotations.and_then(|value| ToolAnnotationsView::deserialize(value).ok())
}

pub fn tool_name_style() -> Style {
    Style::default()
        .fg(crate::theme::accent_color())
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
        crate::theme::error_color()
    } else if has_writes {
        crate::theme::warning_color()
    } else if has_closed_world {
        crate::theme::contained_color()
    } else if has_open_world {
        crate::theme::annotation_color()
    } else if has_read_only {
        crate::theme::accent_color()
    } else if has_idempotent {
        crate::theme::success_color()
    } else {
        crate::theme::accent_color()
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
    let annotations = parsed_annotations(annotations).unwrap_or_default();
    // These flags are idempotent; duplicates need no owned merge or deduplication.
    let extra = [
        annotations
            .read_only_hint
            .map(|read_only| if read_only { "read-only" } else { "writes" }),
        (annotations.destructive_hint == Some(true)).then_some("destructive"),
        (annotations.idempotent_hint == Some(true)).then_some("idempotent"),
        annotations.open_world_hint.map(|open_world| {
            if open_world {
                "open-world"
            } else {
                "closed-world"
            }
        }),
    ];
    style_for_labels(
        labels
            .iter()
            .map(String::as_str)
            .chain(extra.into_iter().flatten()),
    )
}

pub fn tool_name_style_from_annotations(annotations: Option<&serde_json::Value>) -> Style {
    tool_name_style_from_labels_and_annotations(&[], annotations)
}

#[cfg(test)]
pub(crate) mod tests;
