//! Tests for our cache and viewport adapter. Synthetic protocol rows stand in
//! for completed worker output.

use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui_image::protocol::{Protocol, iterm2::Iterm2};

fn image_rows() -> SlicedProtocol {
    SlicedProtocol::Sliced(
        (0..6)
            .map(|row| {
                Protocol::ITerm2(Iterm2 {
                    data: format!("image-row-{row}"),
                    size: Size::new(4, 1),
                    is_tmux: false,
                })
            })
            .collect(),
    )
}

#[test]
fn viewport_clips_both_ends_without_touching_composer_or_neighbor() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 16, 10));
    buf[(0, 5)].set_symbol("composer");
    buf[(12, 2)].set_symbol("neighbor");
    let body = Rect::new(2, 2, 8, 3);
    render_slice(&image_rows(), -2, body, &mut buf);
    assert_eq!(buf[(4, 2)].symbol(), "image-row-2");
    assert_eq!(buf[(4, 3)].symbol(), "image-row-3");
    assert_eq!(buf[(4, 4)].symbol(), "image-row-4");
    assert_eq!(buf[(4, 5)].symbol(), " ");
    assert_eq!(buf[(0, 5)].symbol(), "composer");
    assert_eq!(buf[(12, 2)].symbol(), "neighbor");
}

#[test]
fn invisible_or_too_narrow_diagrams_do_not_write() {
    let area = Rect::new(0, 0, 12, 4);
    let mut buf = Buffer::empty(area);
    let before = buf.clone();
    for row in [i64::MIN, -6, 4, i64::MAX] {
        render_slice(&image_rows(), row, area, &mut buf);
    }
    render_slice(&image_rows(), 0, Rect::new(0, 0, 3, 4), &mut buf);
    assert_eq!(buf, before);
}

#[test]
fn cache_invalidates_images_on_width_change_but_not_unchanged_frames() {
    let mut renderer = MermaidRenderer::new(Picker::halfblocks(), FrameRequester::test_dummy());
    assert!(renderer.refresh(80));
    renderer.entries.push(Entry {
        source: "source".into(),
        state: Cached::Ready(Arc::new(image_rows())),
        used: 0,
    });
    assert!(!renderer.refresh(80));
    assert!(renderer.image("source").is_some());
    assert!(renderer.refresh(40));
    assert!(renderer.image("source").is_none());
}

#[tokio::test]
async fn failed_worker_keeps_source_without_retry_loop() {
    let mut renderer = MermaidRenderer::new(Picker::halfblocks(), FrameRequester::test_dummy());
    renderer.refresh(80);
    let (sender, receiver) = mpsc::channel(1);
    sender
        .send(Err("synthetic failure".into()))
        .await
        .expect("send result");
    renderer.entries.push(Entry {
        source: "source".into(),
        state: Cached::Pending {
            receiver,
            _task: AbortOnDropHandle::new(tokio::spawn(async {})),
        },
        used: 0,
    });
    assert!(renderer.refresh(80));
    renderer.request("source");
    assert!(matches!(renderer.entries[0].state, Cached::Failed));
    assert!(!renderer.refresh(80));
}
