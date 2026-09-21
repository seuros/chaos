//! Bounded, local-only image previews. No terminal queries or graphics escapes.
use anyhow::Result;
use image::{ImageFormat, Rgba};
use libui::image_preview::decode_base64;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};
use ratatui_image::protocol::{StatefulProtocol, StatefulProtocolType};
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{FilterType, FontSize, Resize, StatefulImage};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::task::AbortOnDropHandle;

use crate::tui::FrameRequester;

// A running blocking decoder cannot be aborted. Keep its permit until it exits,
// so rapid selection changes cannot accumulate concurrent decode/resize jobs.
static IMAGE_WORK: Semaphore = Semaphore::const_new(1);

async fn image_work<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = IMAGE_WORK.acquire().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await?
}

pub(super) struct ImagePreview {
    // Abort before dropping the private reply channel. Completed blocking work
    // cannot leak into another resource/process, even if it finishes later.
    _worker: AbortOnDropHandle<()>,
    protocol: ThreadProtocol,
    replies: mpsc::Receiver<Result<ResizeResponse>>,
    error: Option<String>,
}

impl ImagePreview {
    pub(super) async fn decode(blob: String, frames: FrameRequester) -> Result<(Self, String)> {
        let (protocol, label) = image_work(move || {
            let image = decode_base64(
                &blob,
                Some(&[ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP]),
            )?;
            let (width, height) = (image.width(), image.height());
            let protocol = StatefulProtocol::new(
                image.thumbnail(512, 512),
                FontSize::new(1, 2),
                // A neutral matte keeps black and white transparent artwork visible.
                Some(Rgba([128, 128, 128, 255])),
                StatefulProtocolType::Halfblocks(Default::default()),
            );
            Ok((
                protocol,
                format!("Image {width}×{height} · half-block preview"),
            ))
        })
        .await?;
        let (requests_tx, mut requests) = mpsc::unbounded_channel::<ResizeRequest>();
        let (replies_tx, replies) = mpsc::channel(1);
        let worker = tokio::spawn(async move {
            // ThreadProtocol only requests another resize after receiving this one.
            while let Some(request) = requests.recv().await {
                let result = image_work(move || Ok(request.resize_encode()?)).await;
                if replies_tx.send(result).await.is_err() {
                    break;
                }
                frames.schedule_frame();
            }
        });
        Ok((
            Self {
                _worker: AbortOnDropHandle::new(worker),
                protocol: ThreadProtocol::new(requests_tx, Some(protocol)),
                replies,
                error: None,
            },
            label,
        ))
    }

    pub(super) fn render(&mut self, mut area: Rect, buf: &mut Buffer) {
        if let Ok(reply) = self.replies.try_recv() {
            match reply {
                Ok(resized) => {
                    self.protocol.update_resized_protocol(resized);
                }
                Err(error) => {
                    self.error = Some(super::plain_text(
                        &format!("Image preview failed: {error}. Press r to retry."),
                        256,
                    ));
                }
            }
        }
        if let Some(error) = &self.error {
            Paragraph::new(error.as_str()).render(area, buf);
            return;
        }
        // Bound the output allocation even on exceptionally large terminals.
        area.width = area.width.min(160);
        area.height = area.height.min(80);
        StatefulImage::default()
            .resize(Resize::Scale(Some(FilterType::Triangle)))
            .render(area, buf, &mut self.protocol);
        if self.protocol.protocol_type().is_none() {
            Paragraph::new("Rendering image…").render(area, buf);
        }
    }
}
