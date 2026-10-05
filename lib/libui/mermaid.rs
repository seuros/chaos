//! Native, offline Mermaid rendering for the retained chat viewport.
//!
//! Markdown and history remain text. Only this viewport owns terminal graphics;
//! source is always available in the transcript and on unsupported terminals.

use std::sync::{Arc, LazyLock, OnceLock};

use image::DynamicImage;
use ratatui::text::Line;
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::task::AbortOnDropHandle;

use crate::tui::FrameRequester;

/// Retained display content. Fallback lines already include the cell's prefix.
pub enum DisplayChunk {
    Text(Vec<Line<'static>>),
    Mermaid {
        source: String,
        fallback: Vec<Line<'static>>,
    },
}

const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_SOURCE_LINES: usize = 200;
const MAX_NODES: usize = 100;
const MAX_EDGES: usize = 200;
const MAX_PIXELS: u32 = 2_000_000;
const MAX_SIDE: u32 = 1600;
const CACHE_ENTRIES: usize = 8;
static RENDER_SLOT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

pub(crate) fn within_source_limits(source: &str) -> bool {
    source.len() <= MAX_SOURCE_BYTES
        && source.lines().take(MAX_SOURCE_LINES + 1).count() <= MAX_SOURCE_LINES
}

enum Cached {
    Pending {
        receiver: mpsc::Receiver<Result<SlicedProtocol, String>>,
        _task: AbortOnDropHandle<()>,
    },
    Ready(Arc<SlicedProtocol>),
    Failed,
}

struct Entry {
    source: String,
    state: Cached,
    used: u64,
}

/// A small viewport-local LRU. Only visible diagrams request work; resizing or
/// switching sessions drops stale jobs/results. At most one rasterizer runs
/// process-wide, including blocking work whose owner has since been dropped.
pub(crate) struct MermaidRenderer {
    picker: Picker,
    requester: FrameRequester,
    entries: Vec<Entry>,
    width: u16,
    clock: u64,
    changed: bool,
}

impl MermaidRenderer {
    pub(crate) fn new(picker: Picker, requester: FrameRequester) -> Self {
        Self {
            picker,
            requester,
            entries: Vec::new(),
            width: 0,
            clock: 0,
            changed: false,
        }
    }

    pub(crate) fn refresh(&mut self, width: u16) -> bool {
        let width = width.saturating_sub(2).min(160);
        if self.width != width {
            self.width = width;
            self.entries.clear();
            self.changed = true;
        }
        for entry in &mut self.entries {
            if let Cached::Pending { receiver, .. } = &mut entry.state {
                let result = match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        Some(Err("worker stopped".into()))
                    }
                    Err(mpsc::error::TryRecvError::Empty) => None,
                };
                if let Some(result) = result {
                    entry.state = match result {
                        Ok(image) => Cached::Ready(Arc::new(image)),
                        Err(error) => {
                            tracing::debug!(%error, "Mermaid source fallback");
                            Cached::Failed
                        }
                    };
                    self.changed = true;
                }
            }
        }
        std::mem::take(&mut self.changed)
    }

    pub(crate) fn image(&self, source: &str) -> Option<Arc<SlicedProtocol>> {
        self.entries.iter().find_map(|entry| {
            if entry.source == source
                && let Cached::Ready(image) = &entry.state
            {
                Some(image.clone())
            } else {
                None
            }
        })
    }

    pub(crate) fn begin_frame<'a>(&mut self, visible: impl Iterator<Item = &'a str>) {
        self.clock = self.clock.wrapping_add(1);
        for source in visible {
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.source == source) {
                entry.used = self.clock;
            }
        }
    }

    pub(crate) fn request(&mut self, source: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.source == source) {
            entry.used = self.clock;
            return;
        }
        if self.width < 12 || !within_source_limits(source) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Ok(permit) = RENDER_SLOT.clone().try_acquire_owned() else {
            // A previous viewport's worker may use a different redraw requester.
            // Retry when its permit may be available.
            self.requester
                .schedule_frame_in(std::time::Duration::from_millis(100));
            return;
        };
        if self.entries.len() == CACHE_ENTRIES {
            let Some(index) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.used != self.clock)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
            else {
                // All cached diagrams are on screen. Keep additional ones as
                // source rather than endlessly evicting and rendering them.
                return;
            };
            self.entries.remove(index);
            self.changed = true;
        }
        let picker = self.picker.clone();
        let width = self.width;
        let input = source.to_owned();
        let requester = self.requester.clone();
        let (tx, receiver) = mpsc::channel(1);
        let task = runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                render(&input, &picker, width)
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = tx.send(result).await;
            requester.schedule_frame();
        });
        self.entries.push(Entry {
            source: source.to_owned(),
            state: Cached::Pending {
                receiver,
                _task: AbortOnDropHandle::new(task),
            },
            used: self.clock,
        });
    }
}

fn rasterize(source: &str, font: FontSize, columns: u16) -> Result<DynamicImage, String> {
    if !within_source_limits(source) {
        return Err("diagram exceeds source limits".into());
    }
    let mut parsed = mermaid_rs_renderer::parse_mermaid(source).map_err(|e| e.to_string())?;
    if parsed.graph.nodes.len() > MAX_NODES
        || parsed.graph.edges.len() > MAX_EDGES
        || parsed.graph.subgraphs.len() > 20
    {
        return Err("diagram exceeds layout limits".into());
    }
    parsed.graph.node_links.clear();
    let theme = mermaid_rs_renderer::Theme::modern();
    let config = mermaid_rs_renderer::LayoutConfig::default();
    let layout = mermaid_rs_renderer::compute_layout(&parsed.graph, &theme, &config);
    let svg = mermaid_rs_renderer::render_svg(&layout, &theme, &config);
    if svg.len() > 2 * 1024 * 1024 {
        return Err("diagram exceeds SVG limit".into());
    }
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut fonts = resvg::usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        #[cfg(target_os = "macos")]
        fonts.set_sans_serif_family("Arial");
        Arc::new(fonts)
    });
    let options = resvg::usvg::Options {
        fontdb: fonts.clone(),
        // Keep generated SVG self-contained: reject embedded and external
        // image references.
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&svg, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let max_width = (u32::from(columns) * u32::from(font.width)).min(MAX_SIDE);
    let scale = (f32::from(font.height) / 20.0)
        .min(max_width as f32 / size.width())
        .min(MAX_SIDE as f32 / size.height());
    // Shrinking dense diagrams into illegible thumbnails is worse than source.
    if !scale.is_finite() || scale < 0.5 {
        return Err("diagram is too large for readable labels".into());
    }
    let width = (size.width() * scale).ceil() as u32;
    let height = (size.height() * scale).ceil() as u32;
    if width == 0 || height == 0 || width.saturating_mul(height) > MAX_PIXELS {
        return Err("diagram exceeds raster limit".into());
    }
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).ok_or("could not allocate diagram")?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let image =
        image::RgbaImage::from_raw(width, height, pixmap.take()).ok_or("invalid diagram raster")?;
    Ok(DynamicImage::ImageRgba8(image))
}

fn render(source: &str, picker: &Picker, columns: u16) -> Result<SlicedProtocol, String> {
    let image = rasterize(source, picker.font_size(), columns)?;
    SlicedProtocol::new(picker, image, None).map_err(|error| error.to_string())
}

pub(crate) fn render_slice(
    image: &SlicedProtocol,
    row: i64,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    use ratatui::widgets::Widget;
    use ratatui_image::sliced::{SignedPosition, SlicedImage};
    let area = ratatui::layout::Rect {
        x: area.x.saturating_add(2.min(area.width)),
        width: area.width.saturating_sub(2),
        ..area
    };
    if area.is_empty()
        || image.size().width > area.width
        || row >= i64::from(area.height)
        || row <= -i64::from(image.size().height)
    {
        return;
    }
    let skip = (-row).max(0) as usize;
    let y = row.max(0) as u16;
    let height = image
        .size()
        .height
        .saturating_sub(skip as u16)
        .min(area.height.saturating_sub(y));
    if height == 0 {
        return;
    }
    if let SlicedProtocol::Sliced(slices) = image {
        // Clip the generic row protocol at both ends; v12-rc's slice count
        // includes rows clipped above the viewport.
        for (offset, slice) in slices
            .iter()
            .skip(skip)
            .take(usize::from(height))
            .enumerate()
        {
            ratatui_image::Image::new(slice).render(
                ratatui::layout::Rect::new(area.x, area.y + y + offset as u16, area.width, 1),
                buf,
            );
        }
    } else {
        SlicedImage::new(image, SignedPosition::from((0, row as i16))).render(area, buf);
    }
}

#[cfg(test)]
mod tests;
