use std::path::Path;

use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FunctionCallOutputContentItem;
use chaos_ipc::models::ImageDetail;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::image_close_tag_text;
use chaos_ipc::models::image_open_tag_text;
use chaos_ipc::models::local_image_open_tag_text;
use chaos_ipc::user_input::UserInput;
use chaos_pixbuf::PromptImageMode;
use chaos_pixbuf::error::ImageProcessingError;
use chaos_pixbuf::load_for_prompt;

pub(crate) fn response_input_item_from_user_input(items: Vec<UserInput>) -> ResponseInputItem {
    let mut image_index = 0;
    ResponseInputItem::Message {
        role: "user".to_string(),
        content: items
            .into_iter()
            .flat_map(|item| match item {
                UserInput::Text { text, .. } => vec![ContentItem::InputText { text }],
                UserInput::Image { image_url } => {
                    image_index += 1;
                    remote_image_content_items(image_url)
                }
                UserInput::LocalImage { path } => {
                    image_index += 1;
                    local_image_content_items_with_label_number(
                        &path,
                        Some(image_index),
                        PromptImageMode::ResizeToFit,
                    )
                }
                UserInput::Mention { .. } => Vec::new(),
                UserInput::EmbeddedResource { name, text, .. } => {
                    let header = format!("[{name}]\n");
                    vec![ContentItem::InputText {
                        text: format!("{header}{text}"),
                    }]
                }
                _ => Vec::new(),
            })
            .collect(),
    }
}

pub(crate) fn local_image_content_items_with_label_number(
    path: &Path,
    label_number: Option<usize>,
    mode: PromptImageMode,
) -> Vec<ContentItem> {
    match load_for_prompt(path, mode) {
        Ok(image) => {
            let mut items = Vec::with_capacity(3);
            if let Some(label_number) = label_number {
                items.push(ContentItem::InputText {
                    text: local_image_open_tag_text(label_number),
                });
            }
            items.push(ContentItem::InputImage {
                image_url: image.into_data_url(),
            });
            if label_number.is_some() {
                items.push(ContentItem::InputText {
                    text: image_close_tag_text(),
                });
            }
            items
        }
        Err(err) => local_image_error_content_items(path, &err),
    }
}

pub(crate) fn local_image_tool_output_items(
    path: &Path,
    mode: PromptImageMode,
    detail: Option<ImageDetail>,
) -> Vec<FunctionCallOutputContentItem> {
    local_image_content_items_with_label_number(path, None, mode)
        .into_iter()
        .map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                FunctionCallOutputContentItem::InputText { text }
            }
            ContentItem::InputImage { image_url } => {
                FunctionCallOutputContentItem::InputImage { image_url, detail }
            }
            ContentItem::Document { name, text, .. } => {
                let header = name.map(|n| format!("[{n}]\n")).unwrap_or_default();
                FunctionCallOutputContentItem::InputText {
                    text: format!("{header}{text}"),
                }
            }
        })
        .collect()
}

fn remote_image_content_items(image_url: String) -> Vec<ContentItem> {
    vec![
        ContentItem::InputText {
            text: image_open_tag_text(),
        },
        ContentItem::InputImage { image_url },
        ContentItem::InputText {
            text: image_close_tag_text(),
        },
    ]
}

fn local_image_error_content_items(path: &Path, err: &ImageProcessingError) -> Vec<ContentItem> {
    if matches!(err, ImageProcessingError::Read { .. }) {
        return vec![ContentItem::InputText {
            text: format!(
                "Chaos could not read the local image at `{}`: {}",
                path.display(),
                err
            ),
        }];
    }

    if err.is_invalid_image() {
        return vec![ContentItem::InputText {
            text: format!("Image located at `{}` is invalid: {}", path.display(), err),
        }];
    }

    let Some(mime_guess) = mime_guess::from_path(path).first() else {
        return vec![ContentItem::InputText {
            text: format!(
                "Chaos could not read the local image at `{}`: unsupported MIME type (unknown)",
                path.display()
            ),
        }];
    };
    let mime = mime_guess.essence_str().to_owned();
    if !mime.starts_with("image/") {
        return vec![ContentItem::InputText {
            text: format!(
                "Chaos could not read the local image at `{}`: unsupported MIME type `{mime}`",
                path.display()
            ),
        }];
    }

    vec![ContentItem::InputText {
        text: format!(
            "Chaos cannot attach image at `{}`: unsupported image format `{mime}`.",
            path.display()
        ),
    }]
}

#[cfg(test)]
mod tests;
