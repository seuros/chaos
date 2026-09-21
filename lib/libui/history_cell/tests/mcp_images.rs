use super::*;
use base64::Engine;
use image::DynamicImage;
use image::ImageFormat;
use image::Rgba;
use image::RgbaImage;
use pretty_assertions::assert_eq;
use std::io::Cursor;

pub(super) fn run() {
    half_blocks_render_both_pixels_and_transparency();
    previews_reflow_with_bounded_dimensions();
    decoding_rejects_invalid_and_oversized_images();
}

#[expect(clippy::disallowed_methods, reason = "Source image pixel assertions")]
fn half_blocks_render_both_pixels_and_transparency() {
    let mut image = RgbaImage::new(2, 3);
    image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    image.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
    image.put_pixel(1, 0, Rgba([0, 0, 0, 255]));
    image.put_pixel(1, 1, Rgba([255, 255, 255, 255]));
    image.put_pixel(0, 2, Rgba([0, 0, 0, 0]));
    image.put_pixel(1, 2, Rgba([0, 0, 0, 128]));
    let cell: Box<dyn HistoryCell> = Box::new(state::CompletedMcpToolCallWithImageOutput::new(
        DynamicImage::ImageRgba8(image),
    ));
    let area = Rect::new(0, 0, 40, HistoryCell::desired_height(cell.as_ref(), 40));
    assert_eq!(area.height, 3);
    let mut buf = Buffer::empty(area);
    cell.render(area, &mut buf);
    assert_eq!(buf[(0, 1)].symbol(), "▀");
    assert_eq!(buf[(0, 1)].fg, Color::Rgb(255, 0, 0));
    assert_eq!(buf[(0, 1)].bg, Color::Rgb(0, 0, 255));
    assert_eq!(buf[(1, 1)].fg, Color::Rgb(0, 0, 0));
    assert_eq!(buf[(1, 1)].bg, Color::Rgb(255, 255, 255));
    assert_eq!(buf[(0, 2)].fg, Color::Rgb(128, 128, 128));
    assert_eq!(buf[(1, 2)].fg, Color::Rgb(64, 64, 64));
    // Odd image heights pad the unused bottom half with the neutral matte.
    assert_eq!(buf[(0, 2)].bg, Color::Rgb(128, 128, 128));

    let clipped_area = Rect::new(0, 0, 40, 1);
    let mut clipped = Buffer::empty(clipped_area);
    cell.render(clipped_area, &mut clipped);
    assert_eq!(clipped[(1, 0)], buf[(1, 2)]);
}

fn previews_reflow_with_bounded_dimensions() {
    let cell = state::CompletedMcpToolCallWithImageOutput::new(DynamicImage::new_rgba8(320, 160));
    let wide = cell.display_lines(100);
    assert_eq!(wide[0].to_string(), "Image 320×160 · preview");
    assert_eq!(wide[1].width(), 80);
    assert_eq!(wide.len(), 21);
    let narrow = cell.display_lines(20);
    assert_eq!(narrow[1].width(), 20);
    assert_eq!(narrow.len(), 6);
    for width in [0, 1, 2, 12, 20, 80, u16::MAX] {
        let lines = cell.display_lines(width);
        assert!(lines.len() <= 25);
        assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
        assert_eq!(cell.desired_height(width) as usize, lines.len());
        assert_eq!(cell.desired_transcript_height(width) as usize, lines.len());
        assert_eq!(cell.transcript_lines(width), lines);
    }
    assert!(cell.display_lines(0).is_empty());
    assert_eq!(
        cell.display_lines(100),
        wide,
        "resizing must not degrade the stored thumbnail"
    );

    let tall = state::CompletedMcpToolCallWithImageOutput::new(DynamicImage::new_rgba8(100, 1000));
    assert_eq!(tall.display_lines(u16::MAX).len(), 25);
}

fn png_block(image: DynamicImage) -> serde_json::Value {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    image_block(&base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()))
}

fn decoding_rejects_invalid_and_oversized_images() {
    for block in [
        text_block("not an image"),
        image_block("not-base64"),
        image_block("bm90IGFuIGltYWdl"),
        json!({"type": "image", "data": 42}),
        image_block(&"A".repeat((4 * 1024 * 1024usize).div_ceil(3) * 4 + 4)),
        png_block(DynamicImage::new_rgba8(4097, 1)),
        png_block(DynamicImage::new_rgba8(3000, 3000)),
    ] {
        assert!(render::decode_mcp_image(&block).is_none());
    }
    let result = Ok(CallToolResult {
        content: vec![text_block("Caption"), image_block("invalid")],
        is_error: None,
        structured_content: None,
        meta: None,
    });
    assert!(render::try_new_completed_mcp_tool_call_with_image_output(&result).is_none());
    assert!(
        render::try_new_completed_mcp_tool_call_with_image_output(&Err("failed".into())).is_none()
    );
}
