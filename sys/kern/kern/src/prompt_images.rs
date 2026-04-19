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
mod tests {
    use anyhow::Result;
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseInputItem;
    use chaos_ipc::models::image_close_tag_text;
    use chaos_ipc::models::image_open_tag_text;
    use chaos_ipc::models::local_image_open_tag_text;
    use chaos_ipc::user_input::UserInput;
    use chaos_pixbuf::PromptImageMode;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::response_input_item_from_user_input;
    use crate::prompt_images::local_image_content_items_with_label_number;

    #[test]
    fn wraps_remote_and_local_images_in_order() -> Result<()> {
        let image_url = "data:image/png;base64,abc".to_string();
        let dir = tempdir()?;
        let local_path = dir.path().join("local.png");
        const TINY_PNG_BYTES: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2,
            0, 0, 5, 0, 1, 122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ];
        std::fs::write(&local_path, TINY_PNG_BYTES)?;

        let item = response_input_item_from_user_input(vec![
            UserInput::Image {
                image_url: image_url.clone(),
            },
            UserInput::LocalImage { path: local_path },
        ]);

        let ResponseInputItem::Message { content, .. } = item else {
            panic!("expected message response");
        };

        assert_eq!(content.len(), 6);
        assert_eq!(
            content[0],
            ContentItem::InputText {
                text: image_open_tag_text(),
            }
        );
        assert_eq!(content[1], ContentItem::InputImage { image_url });
        assert_eq!(
            content[2],
            ContentItem::InputText {
                text: image_close_tag_text(),
            }
        );
        assert_eq!(
            content[3],
            ContentItem::InputText {
                text: local_image_open_tag_text(2),
            }
        );
        assert!(matches!(
            content.get(4),
            Some(ContentItem::InputImage { .. })
        ));
        assert_eq!(
            content[5],
            ContentItem::InputText {
                text: image_close_tag_text(),
            }
        );

        Ok(())
    }

    #[test]
    fn local_image_errors_become_text_placeholders() -> Result<()> {
        let dir = tempdir()?;
        let missing_path = dir.path().join("missing-image.png");

        let items = local_image_content_items_with_label_number(
            &missing_path,
            None,
            PromptImageMode::ResizeToFit,
        );

        assert_eq!(items.len(), 1);
        let ContentItem::InputText { text } = &items[0] else {
            panic!("expected placeholder text");
        };
        assert!(text.contains("could not read"));
        assert!(text.contains(&missing_path.display().to_string()));

        Ok(())
    }
}
