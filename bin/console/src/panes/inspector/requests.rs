use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chaos_kern::Process;
use chaos_mcp_runtime::ResourceContents;
use tokio::sync::oneshot;
use tokio_util::task::AbortOnDropHandle;

use super::{ImagePreview, InspectorPane, Resource, plain_text};
use crate::tui::FrameRequester;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESOURCES: usize = 128;
const MAX_CONTENT_CHARS: usize = 32_768;

pub(super) enum Update {
    Catalog(Vec<Resource>, String),
    Content(String, Option<Box<ImagePreview>>),
}

pub(super) struct PendingRequest {
    _task: AbortOnDropHandle<()>,
    result: oneshot::Receiver<Update>,
}

struct RequestCompletion {
    sender: Option<oneshot::Sender<Update>>,
    frames: FrameRequester,
}

impl Drop for RequestCompletion {
    fn drop(&mut self) {
        // Wake even on worker failure, after closing its reply channel.
        self.sender.take();
        self.frames.schedule_frame();
    }
}

impl PendingRequest {
    pub(super) fn spawn(
        future: impl Future<Output = Update> + Send + 'static,
        frames: FrameRequester,
    ) -> Self {
        let (sender, result) = oneshot::channel();
        let mut completion = RequestCompletion {
            sender: Some(sender),
            frames,
        };
        let task = tokio::spawn(async move {
            let update = future.await;
            if let Some(sender) = completion.sender.take() {
                let _ = sender.send(update);
            }
        });
        Self {
            _task: AbortOnDropHandle::new(task),
            result,
        }
    }
}

impl InspectorPane {
    pub(super) fn poll_request(&mut self) {
        let Some(request) = self.pending.as_mut() else {
            return;
        };
        let result = match request.result.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => return,
            result => result,
        };
        self.pending = None;
        let error = match result {
            Ok(Update::Catalog(resources, status)) => self
                .set_catalog(resources, status)
                .err()
                .map(|err| err.to_string()),
            Ok(Update::Content(text, image)) => {
                self.json = if image.is_none() {
                    super::JsonPreview::parse(&text)
                } else {
                    None
                };
                self.content = text;
                self.image = image;
                self.content_loaded = true;
                None
            }
            Err(err) => Some(err.to_string()),
        };
        if let Some(error) = error {
            self.catalog_status = format!("Resource worker failed: {error}. Press r to retry.");
            // A failed worker must not turn redraws into automatic retries.
            self.catalog_loaded = true;
            self.content_loaded = true;
        }
    }

    /// Called before rendering. Polling never awaits with the shared pane borrowed.
    pub fn tick(&mut self, process: Arc<Process>, frames: FrameRequester) {
        let (revision, servers) = process.mcp_resource_servers();
        self.sync_mcp_servers(revision, &servers);
        self.poll_request();
        if self.pending.is_some() || !self.needs_poll() {
            return;
        }
        if !self.catalog_loaded {
            self.pending = Some(PendingRequest::spawn(
                async move {
                    let mut resources = Vec::new();
                    let mut errors = Vec::new();
                    for server in servers {
                        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
                            let mut cursor = None;
                            // Bound pagination even if a buggy server repeats its cursor.
                            for _ in 0..8 {
                                let page = process.list_mcp_resources(&server, cursor).await?;
                                for item in page.resources {
                                    if resources.len() == MAX_RESOURCES {
                                        break;
                                    }
                                    if item.uri.len() <= 4096 {
                                        resources.push(Resource {
                                            server: server.clone(),
                                            uri: item.uri,
                                            name: plain_text(&item.name, 128),
                                        });
                                    }
                                }
                                cursor = page.next_cursor;
                                if cursor.is_none() || resources.len() == MAX_RESOURCES {
                                    break;
                                }
                            }
                            anyhow::Ok(())
                        })
                        .await;
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => errors.push(format!("{server}: {err}")),
                            Err(_) => errors.push(format!("{server}: timed out")),
                        }
                        if resources.len() == MAX_RESOURCES {
                            errors.push("Resource list limited to 128 entries.".into());
                            break;
                        }
                    }
                    Update::Catalog(resources, plain_text(&errors.join("\n"), 2048))
                },
                frames,
            ));
        } else if let Some(resource) = self.selected_resource().cloned() {
            let content_frames = frames.clone();
            self.pending = Some(PendingRequest::spawn(
                async move {
                    let result = tokio::time::timeout(
                        REQUEST_TIMEOUT,
                        process.read_mcp_resource(&resource.server, resource.uri),
                    )
                    .await;
                    match result {
                        Ok(Ok(result)) => resource_content(result.contents, content_frames).await,
                        Ok(Err(err)) => {
                            Update::Content(plain_text(&format!("Read failed: {err}"), 2048), None)
                        }
                        Err(_) => Update::Content(
                            "Resource read timed out. Press r to retry.".into(),
                            None,
                        ),
                    }
                },
                frames,
            ));
        }
    }
}

pub(super) async fn resource_content(
    contents: Vec<ResourceContents>,
    frames: FrameRequester,
) -> Update {
    let mut text = String::new();
    let mut image = None;
    let mut image_attempted = false;
    let mut remaining = MAX_CONTENT_CHARS;
    for content in contents {
        let value = match content {
            ResourceContents::Text(value) => value.text,
            ResourceContents::Blob(value)
                if value
                    .mime_type
                    .as_deref()
                    .is_none_or(|mime| mime.starts_with("image/")) =>
            {
                if image_attempted {
                    "[Additional image omitted]".into()
                } else {
                    image_attempted = true;
                    match tokio::time::timeout(
                        REQUEST_TIMEOUT,
                        ImagePreview::decode(value.blob, frames.clone()),
                    )
                    .await
                    {
                        Ok(Ok((preview, label))) => {
                            image = Some(Box::new(preview));
                            label
                        }
                        Ok(Err(error)) => format!("[Image preview unavailable: {error}]"),
                        Err(_) => "[Image preview timed out. Press r to retry.]".into(),
                    }
                }
            }
            ResourceContents::Blob(_) => "[Binary resource omitted]".into(),
        };
        let bounded = plain_text(&value, remaining);
        remaining = remaining.saturating_sub(bounded.chars().count() + 1);
        text.push_str(&bounded);
        text.push('\n');
        if remaining == 0 {
            text.push_str("[Content truncated]");
            break;
        }
    }
    Update::Content(text, image)
}
