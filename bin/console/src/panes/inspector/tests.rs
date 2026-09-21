use super::requests::Update;
use super::*;
use crate::tui::FrameRequester;
use base64::Engine;
use chaos_mcp_runtime::{ResourceContents, ResourceContentsText};
use ratatui_hypertile::{KeyChord, MouseEvent, PaneId};

fn image_resource(width: u32, height: u32) -> ResourceContents {
    png_resource(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([17, 33, 77, 255]),
    ))
}

fn png_resource(image: image::RgbaImage) -> ResourceContents {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    blob_resource(
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        "image/png",
    )
}

fn blob_resource(blob: String, mime_type: &str) -> ResourceContents {
    serde_json::from_value(serde_json::json!({
        "uri": "state://shared", "mimeType": mime_type, "blob": blob,
    }))
    .unwrap()
}

fn resource(server: &str) -> Resource {
    Resource {
        server: server.into(),
        uri: "state://shared".into(),
        name: format!("{server} state"),
    }
}

fn key(pane: &mut InspectorPane, code: KeyCode) {
    assert_eq!(
        pane.handle_event(&HypertileEvent::Key(KeyChord::new(code))),
        EventOutcome::Consumed
    );
}

async fn wait_for_draw(draws: &mut tokio::sync::broadcast::Receiver<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(5), draws.recv())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn inspector_tree_navigation_preserves_resource_and_json_policies() {
    let area = Rect::new(0, 0, 36, 20);
    let mut pane = InspectorPane::default();
    pane.set_catalog(vec![resource("a"), resource("b")], String::new())
        .unwrap();
    pane.render(area, &mut Buffer::empty(area), true);
    let opened = pane.tree.opened().clone();
    key(&mut pane, KeyCode::Right);
    assert_eq!(pane.tree.selected(), &["a", "state://shared"]);
    assert_eq!(pane.tree.opened(), &opened, "resource leaves stay closed");
    for (code, selected) in [
        (KeyCode::Up, vec!["a"]),
        (KeyCode::Right, vec!["a"]),
        (KeyCode::End, vec!["b", "state://shared"]),
        (KeyCode::Home, vec!["a"]),
        (KeyCode::Down, vec!["a", "state://shared"]),
        (KeyCode::Left, vec!["a"]),
    ] {
        key(&mut pane, code);
        assert_eq!(pane.tree.selected(), selected, "{code:?}");
    }
    key(&mut pane, KeyCode::PageDown);
    assert_eq!(pane.scroll, pane.preview_area.height);
    assert_eq!(pane.tree.selected(), &["a"]);
    key(&mut pane, KeyCode::PageUp);
    assert_eq!(pane.scroll, 0);

    let mut json = JsonPreview::parse(r#"{"a":1,"b":2}"#).unwrap();
    json.render(area, &mut Buffer::empty(area), true);
    for (code, selected) in [
        (KeyCode::Down, vec![0, 0]),
        (KeyCode::Up, vec![0]),
        (KeyCode::End, vec![0, 1]),
        (KeyCode::Home, vec![0]),
        (KeyCode::PageDown, vec![0, 1]),
        (KeyCode::PageUp, vec![0]),
    ] {
        assert!(json.key(code));
        assert_eq!(json.state.selected(), selected, "{code:?}");
    }
    assert!(json.key(KeyCode::Left));
    assert!(!json.state.opened().contains(&vec![0]));
    assert!(json.key(KeyCode::Right));
    assert!(json.state.opened().contains(&vec![0]));
    assert!(!json.key(KeyCode::Char('x')));
}

#[tokio::test]
async fn inspector_resource_navigation_does_not_retain_another_sessions_content() {
    let mut pane = InspectorPane::default();
    pane.set_process(Some(ProcessId::new()));
    pane.sync_mcp_servers(0, &[]);
    pane.set_catalog(vec![], String::new()).unwrap();
    assert!(!pane.needs_poll());
    // Mounting a server invalidates even a previously empty catalog, and discards
    // replies already queued by the old generation before they can be applied.
    pane.pending = Some(PendingRequest::spawn(
        async { Update::Catalog(vec![], "stale catalog".into()) },
        FrameRequester::test_dummy(),
    ));
    tokio::task::yield_now().await;
    let servers = vec!["a".into(), "b".into()];
    pane.sync_mcp_servers(1, &servers);
    pane.poll_request();
    assert!(pane.pending.is_none());
    assert!(!pane.catalog_loaded);
    assert!(pane.catalog_status.is_empty());
    assert!(pane.needs_poll());
    pane.set_catalog(
        vec![resource("b"), resource("a"), resource("a")],
        String::new(),
    )
    .unwrap();
    let area = Rect::new(0, 0, 36, 20);
    pane.render(area, &mut Buffer::empty(area), true);
    assert_eq!(
        pane.resources.len(),
        2,
        "deduplicate within a server, not across servers"
    );
    assert_eq!(pane.selected_resource().unwrap().server, "a");
    pane.content = "old resource".into();
    pane.json = JsonPreview::parse(r#"{"old":true}"#);
    pane.content_loaded = true;
    pane.scroll = 2;
    key(&mut pane, KeyCode::Down);
    key(&mut pane, KeyCode::Down);
    assert_eq!(pane.selected_resource().unwrap().server, "b");
    assert!(pane.content.is_empty());
    assert!(pane.json.is_none());
    assert!(!pane.content_loaded);
    assert_eq!(pane.scroll, 0);
    pane.tree.close(&["a".into()]);
    pane.set_catalog(vec![resource("a"), resource("b")], String::new())
        .unwrap();
    assert_eq!(pane.selected_resource().unwrap().server, "b");
    assert!(!pane.tree.opened().contains(&vec!["a".into()]));

    pane.content = "cached preview".into();
    pane.content_loaded = true;
    pane.sync_mcp_servers(1, &servers);
    assert_eq!(pane.content, "cached preview");
    assert!(!pane.needs_poll(), "unchanged registry must not reread");
    pane.pending = Some(PendingRequest::spawn(
        async { Update::Content("stale before reset".into(), None) },
        FrameRequester::test_dummy(),
    ));
    tokio::task::yield_now().await;
    pane.sync_mcp_servers(2, &servers); // reset with unchanged names
    pane.poll_request();
    assert!(pane.content.is_empty());
    assert!(pane.pending.is_none());
    assert!(!pane.catalog_loaded);
    pane.set_catalog(vec![resource("a"), resource("b")], String::new())
        .unwrap();
    assert_eq!(pane.selected_resource().unwrap().server, "b");
    assert!(!pane.tree.opened().contains(&vec!["a".into()]));

    // A reply already queued before a selection change must also be discarded.
    pane.pending = Some(PendingRequest::spawn(
        async { Update::Content("stale".into(), None) },
        FrameRequester::test_dummy(),
    ));
    tokio::task::yield_now().await;
    key(&mut pane, KeyCode::Left);
    pane.poll_request();
    assert!(pane.content.is_empty());
    assert!(
        !pane.needs_poll(),
        "selecting a server must not issue a read"
    );

    // Removing the selected server falls back to a live identity, even if collapsed.
    pane.sync_mcp_servers(3, &["a".into()]);
    pane.set_catalog(vec![resource("a")], "b unavailable".into())
        .unwrap();
    assert_eq!(pane.tree.selected(), &["a"]);
    assert!(!pane.tree.opened().contains(&vec!["a".into()]));

    // The plugin lifecycle cancels pending work when hidden/unmounted.
    let state = Rc::new(RefCell::new(pane));
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    state.borrow_mut().pending = Some(PendingRequest::spawn(
        async move {
            let _ = receiver.await;
            Update::Content("cancelled".into(), None)
        },
        FrameRequester::test_dummy(),
    ));
    InspectorPlugin(state.clone()).on_unmount(PluginContext {
        pane_id: PaneId::ROOT,
    });
    tokio::task::yield_now().await;
    assert!(sender.send(()).is_err());
    state.borrow_mut().pending = Some(PendingRequest::spawn(
        async { Update::Content("reply from the previous process".into(), None) },
        FrameRequester::test_dummy(),
    ));
    tokio::task::yield_now().await;
    let mut pane = state.borrow_mut();
    pane.content = "private session data".into();
    pane.set_process(Some(ProcessId::new()));
    pane.poll_request();
    assert!(pane.mcp_snapshot.is_none());
    assert!(pane.resources.is_empty());
    assert!(pane.items.is_empty());
    assert!(pane.tree.selected().is_empty());
    assert!(pane.tree.opened().is_empty());
    assert!(pane.content.is_empty());
    assert!(!pane.catalog_loaded);
    pane.set_catalog(vec![], "server timed out".into()).unwrap();
    assert!(
        !pane.needs_poll(),
        "empty/error catalogs must not retry on redraw"
    );
    key(&mut pane, KeyCode::Char('r'));
    assert!(pane.needs_poll());
    let text = plain_text("\u{1b}[2Jhello\u{7}\nworld", 9);
    assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'));
    assert_eq!(text.chars().count(), 9);
}

#[tokio::test]
#[expect(clippy::disallowed_methods, reason = "Source image pixel assertions")]
async fn inspector_mouse_targeting_and_resource_preview() {
    let mut pane = InspectorPane::default();
    pane.set_catalog(vec![resource("a"), resource("b")], String::new())
        .unwrap();
    pane.content = (0..30).map(|i| format!("line {i}\n")).collect();
    let area = Rect::new(0, 0, 36, 18);
    pane.render(area, &mut Buffer::empty(area), true);
    let selected = pane.tree.selected().to_vec();
    let pointer = pane.preview_area.as_position();
    pane.handle_event(&HypertileEvent::Mouse(MouseEvent::new(
        MouseEventKind::ScrollDown,
        pointer.x,
        pointer.y,
    )));
    assert_eq!(pane.scroll, 1);
    assert_eq!(pane.tree.selected(), selected);
    let pointer = pane.tree_area.as_position();
    pane.handle_event(&HypertileEvent::Mouse(MouseEvent::new(
        MouseEventKind::Down(MouseButton::Left),
        pointer.x,
        pointer.y,
    )));
    assert_eq!(pane.tree.selected(), &["a"]);
    assert!(pane.content.is_empty());
    key(&mut pane, KeyCode::Left);
    assert!(!pane.tree.opened().contains(&vec!["a".into()]));
    key(&mut pane, KeyCode::Right);
    assert!(pane.tree.opened().contains(&vec!["a".into()]));

    // Exercise our content policy and threaded image integration in the same pane.
    let (draw_tx, mut draws) = tokio::sync::broadcast::channel(8);
    let frames = FrameRequester::new(draw_tx);
    let contents = vec![
        ResourceContents::Text(ResourceContentsText {
            uri: "state://shared".into(),
            mime_type: None,
            text: "caption\u{7}".into(),
            meta: None,
        }),
        image_resource(4, 4),
        image_resource(4, 4),
        blob_resource("not an image".into(), "application/octet-stream"),
    ];
    pane.render(area, &mut Buffer::empty(area), true);
    key(&mut pane, KeyCode::Down);
    pane.pending = Some(PendingRequest::spawn(
        requests::resource_content(contents, frames.clone()),
        frames.clone(),
    ));
    wait_for_draw(&mut draws).await;
    pane.poll_request();
    assert!(pane.content.starts_with("caption\nImage 4×4"));
    assert!(pane.content.contains("[Additional image omitted]"));
    assert!(pane.content.contains("[Binary resource omitted]"));
    let image_color = ratatui::style::Color::Rgb(17, 33, 77);
    for (width, height) in [(36, 18), (24, 14), (14, 8)] {
        let area = Rect::new(3, 2, width, height);
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 30));
        pane.render(area, &mut buf, true);
        wait_for_draw(&mut draws).await;
        pane.render(area, &mut buf, true);
        let image_cells: Vec<_> = buf
            .content
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.bg == image_color)
            .map(|(i, _)| buf.pos_of(i))
            .collect();
        assert!(
            !image_cells.is_empty(),
            "preview missing at {width}x{height}"
        );
        assert!(image_cells.iter().all(|&(x, y)| x > area.x
            && x < area.right() - 1
            && y >= pane.preview_text_area.bottom()
            && y < area.bottom() - 1));
        let (x, y) = image_cells[0];
        pane.scroll = 0;
        let selected = pane.tree.selected().to_vec();
        for (kind, expected) in [
            (MouseEventKind::ScrollDown, 1),
            (MouseEventKind::ScrollUp, 0),
        ] {
            assert_eq!(
                pane.handle_event(&HypertileEvent::Mouse(MouseEvent::new(kind, x, y))),
                EventOutcome::Consumed
            );
            assert_eq!(pane.scroll, expected);
            assert_eq!(pane.tree.selected(), selected);
        }
        assert!(
            buf.content
                .iter()
                .all(|cell| !cell.symbol().contains('\u{1b}'))
        );
    }
    pane.render(Rect::ZERO, &mut Buffer::empty(Rect::ZERO), true);
    // Hiding discards the image worker but preserves the selected resource for reread.
    pane.pause();
    assert!(pane.image.is_none());
    assert!(pane.selected_resource().is_some());
    assert!(pane.needs_poll());

    // Transparent logos must retain contrast, without recoloring opaque artwork.
    let colors = [
        [0, 0, 0, 255],
        [0, 0, 0, 0],
        [255, 255, 255, 255],
        [17, 33, 77, 255],
    ];
    let logo = png_resource(image::RgbaImage::from_fn(64, 32, |x, _| {
        image::Rgba(colors[(x / 16) as usize])
    }));
    let Update::Content(_, Some(mut preview)) =
        requests::resource_content(vec![logo], frames.clone()).await
    else {
        panic!("expected image preview");
    };
    let area = Rect::new(3, 2, 64, 16);
    let mut buf = Buffer::empty(area);
    preview.render(area, &mut buf);
    wait_for_draw(&mut draws).await;
    preview.render(area, &mut buf);
    use ratatui::style::Color::Rgb;
    for (i, [r, g, b, alpha]) in colors.into_iter().enumerate() {
        // Sample inside each band, away from interpolated edges.
        let cell = &buf[(area.x + i as u16 * 16 + 8, area.y + 8)];
        let expected = if alpha == 0 {
            Rgb(128, 128, 128)
        } else {
            Rgb(r, g, b)
        };
        assert_eq!((cell.fg, cell.bg), (expected, expected));
    }

    // Invalid images keep an explanatory text preview instead of exposing raw blobs.
    let ResourceContents::Blob(oversize_dimensions) = image_resource(4097, 1) else {
        unreachable!()
    };
    for blob in [
        "!invalid base64!".into(),
        base64::engine::general_purpose::STANDARD.encode(b"GIF89a"),
        base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nbroken"),
        "A".repeat(libui::image_preview::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1),
        oversize_dimensions.blob,
    ] {
        let update =
            requests::resource_content(vec![blob_resource(blob, "image/png")], frames.clone())
                .await;
        assert!(
            matches!(update, Update::Content(text, None) if text.starts_with("[Image preview unavailable:"))
        );
    }

    let Update::Content(text, image) =
        requests::resource_content(vec![image_resource(4, 4)], frames).await
    else {
        panic!("expected content");
    };
    pane.content = text;
    pane.image = image;
    pane.set_process(Some(ProcessId::new()));
    assert!(pane.content.is_empty() && pane.image.is_none());

    pane.set_catalog(vec![resource("a")], String::new())
        .unwrap();
    let (draw_tx, mut draws) = tokio::sync::broadcast::channel(8);
    let frames = FrameRequester::new(draw_tx);
    for text in [
        r#"{"nested":{"array":[1,true,null]},"escape":"\u001b[2J"}"#.to_owned(),
        "{invalid".into(),
        format!("[{}]", vec!["0"; 513].join(",")),
    ] {
        pane.pending = Some(PendingRequest::spawn(
            requests::resource_content(
                vec![ResourceContents::Text(ResourceContentsText {
                    uri: "state://shared".into(),
                    mime_type: Some("application/json".into()),
                    text: text.clone(),
                    meta: None,
                })],
                frames.clone(),
            ),
            frames.clone(),
        ));
        wait_for_draw(&mut draws).await;
        pane.poll_request();
        assert!(pane.content.starts_with(&text));
        if text.starts_with(r#"{"nested""#) {
            let area = Rect::new(3, 2, 48, 18);
            let mut buf = Buffer::empty(area);
            pane.render(area, &mut buf, true);
            key(&mut pane, KeyCode::Char('j'));
            assert!(!pane.json.as_ref().unwrap().visible);
            pane.render(area, &mut buf, true);
            key(&mut pane, KeyCode::Char('j'));
            pane.render(area, &mut buf, true);
            let selected = pane.tree.selected().to_vec();
            key(&mut pane, KeyCode::Enter);
            key(&mut pane, KeyCode::Down);
            key(&mut pane, KeyCode::Right);
            assert_eq!(pane.tree.selected(), selected);
            assert_eq!(pane.json.as_ref().unwrap().state.selected(), &[0, 0]);
            assert!(
                pane.json
                    .as_ref()
                    .unwrap()
                    .state
                    .opened()
                    .contains(&vec![0, 0])
            );
            pane.render(area, &mut buf, true);
            assert!(
                buf.content
                    .iter()
                    .all(|cell| !cell.symbol().contains('\u{1b}'))
            );
            key(&mut pane, KeyCode::Enter);
            key(&mut pane, KeyCode::Left);
            assert!(pane.json.is_none());
        } else {
            assert!(
                pane.json.is_none(),
                "unrenderable JSON retains text fallback"
            );
        }
    }
}
