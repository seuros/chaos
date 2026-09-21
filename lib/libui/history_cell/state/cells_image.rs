//! Line-based image previews survive scrollback, clipping, and transcript reflow.
//! No terminal queries or graphics escape sequences are needed.

use image::DynamicImage;
use image::Rgba;
use ratatui::prelude::*;

use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;

use super::trait_def::HistoryCell;

const MAX_PREVIEW_COLUMNS: u32 = 80;
const MAX_PREVIEW_ROWS: u32 = 24;
const MATTE: u8 = 128;

#[derive(Debug)]
pub(crate) struct CompletedMcpToolCallWithImageOutput {
    // Retain only a small thumbnail, not a full decoded image per history entry.
    preview: DynamicImage,
    label: String,
}

impl CompletedMcpToolCallWithImageOutput {
    pub(crate) fn new(image: DynamicImage) -> Self {
        let label = format!("Image {}×{} · preview", image.width(), image.height());
        let preview = image.thumbnail(
            image.width().min(MAX_PREVIEW_COLUMNS),
            image.height().min(MAX_PREVIEW_ROWS * 2),
        );
        Self { preview, label }
    }
}

impl HistoryCell for CompletedMcpToolCallWithImageOutput {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }

        let mut lines = vec![truncate_line_with_ellipsis_if_overflow(
            Line::from(self.label.clone().dim()),
            usize::from(width),
        )];
        let image = self
            .preview
            .thumbnail(
                u32::from(width).min(self.preview.width()),
                self.preview.height(),
            )
            .to_rgba8();
        // A terminal cell is approximately twice as tall as it is wide. Each
        // upper half-block carries two square pixels via foreground/background.
        for y in (0..image.height()).step_by(2) {
            let spans = (0..image.width())
                .map(|x| {
                    let top = pixel_color(*image.get_pixel(x, y));
                    let bottom = if y + 1 < image.height() {
                        pixel_color(*image.get_pixel(x, y + 1))
                    } else {
                        pixel_color(Rgba([0, 0, 0, 0]))
                    };
                    Span::styled("▀", Style::default().fg(top).bg(bottom))
                })
                .collect::<Vec<_>>();
            lines.push(Line::from(spans));
        }
        lines
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Image pixels require their source RGB colors, not theme colors"
)]
fn pixel_color(Rgba([r, g, b, a]): Rgba<u8>) -> Color {
    // A neutral matte makes transparent black AND white artwork visible on
    // either terminal theme, including the black Prometheus Rust logo.
    let blend = |channel: u8| {
        ((u32::from(channel) * u32::from(a) + u32::from(MATTE) * (255 - u32::from(a)) + 127) / 255)
            as u8
    };
    Color::Rgb(blend(r), blend(g), blend(b))
}
