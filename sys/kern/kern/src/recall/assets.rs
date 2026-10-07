//! Download the pinned recall artifacts through ChaOS's existing HTTP transport.
//! Inference in chaos-recall remains local-only.

use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chaos_client::{Egress, RamaTransport, Request};
use futures::StreamExt;
use rama::http::{Method, StatusCode, header};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub(super) const REVISION: &str = "bf8b056651a2c21b8d2565580b8569da283cab23";
const MODEL: &str = "minishlab/potion-base-8M";
pub(super) type DownloadRoute = Result<Option<Egress>, String>;

pub(super) struct Artifact {
    pub(super) name: &'static str,
    pub(super) size: u64,
    pub(super) sha256: &'static str,
}

// Cross-checked against pinned upstream metadata and the real-model fixture.
pub(super) const MANIFEST: [Artifact; 3] = [
    Artifact {
        name: "config.json",
        size: 202,
        sha256: "2a6ac0e9aaa356a68a5688070db78fc3a464fefe85d2f06a1905ce3718687553",
    },
    Artifact {
        name: "tokenizer.json",
        size: 683_666,
        sha256: "e67e803f624fb4d67dea1c730d06e1067e1b14d830e2c2202569e3ef0f70bb50",
    },
    Artifact {
        name: "model.safetensors",
        size: 30_236_760,
        sha256: "f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2",
    },
];

fn snapshot(root: &Path) -> PathBuf {
    root.join("snapshots").join(REVISION)
}

/// Cache hits only read/validate existing files; no sockets, API client, token
/// reads or cache directory creation occur.
pub(super) async fn ensure(
    cache_root: PathBuf,
    route: DownloadRoute,
    cancellation: CancellationToken,
) -> Result<PathBuf, String> {
    // Keep hashing and filesystem work off the application runtime.
    isolated(cancellation, move || async move {
        let directory = snapshot(&cache_root);
        if verify(&directory, &cache_root, &MANIFEST).is_ok() {
            tracing::info!("recall verified model cache hit");
            return Ok(directory);
        }
        let egress = route.map_err(|reason| format!("Recall model is not cached: {reason}"))?;
        if ambient_proxy() {
            return Err(
                "Recall model is not cached; ambient HTTP proxies are not supported. \
                 Use configured egress_url or a verified managed model snapshot.".into(),
            );
        }
        if std::fs::symlink_metadata(&directory).is_ok() {
            return Err(format!(
                "Recall snapshot is invalid; remove {} and retry, or replace it with verified artifacts",
                directory.display()
            ));
        }
        create_cache_directory(&cache_root)?;
        let snapshots = cache_root.join("snapshots");
        create_cache_directory(&snapshots)?;
        let staging = tempfile::Builder::new()
            .prefix(".download-")
            .tempdir_in(&snapshots)
            .map_err(|e| format!("create recall download directory: {e}"))?;
        let client = RamaTransport::default_client_with_egress(egress);
        tracing::info!("recall downloading pinned model artifacts");
        for artifact in &MANIFEST {
            tracing::info!(artifact = artifact.name, "recall downloading model artifact");
            download(&client, artifact, staging.path()).await?;
        }
        verify(staging.path(), staging.path(), &MANIFEST)?;
        // Another process may already have published the same pinned files.
        if verify(&directory, &cache_root, &MANIFEST).is_ok() {
            return Ok(directory);
        }
        std::fs::rename(staging.path(), &directory)
            .map_err(|e| format!("publish recall model snapshot: {e}"))?;
        verify(&directory, &cache_root, &MANIFEST)?;
        Ok(directory)
    })
    .await
}

async fn download(
    client: &RamaTransport,
    artifact: &Artifact,
    directory: &Path,
) -> Result<(), String> {
    let failure = || format!("Failed to download recall artifact {}", artifact.name);
    let mut url = url::Url::parse(&format!(
        "https://huggingface.co/{MODEL}/resolve/{REVISION}/{}",
        artifact.name
    ))
    .map_err(|_| failure())?;
    for hop in 0..=5 {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() != Some(443)
            || !matches!(
                url.host_str(),
                Some(
                    "huggingface.co"
                        | "cdn-lfs.huggingface.co"
                        | "cdn-lfs.hf.co"
                        | "cdn-lfs-us-1.hf.co"
                        | "cdn-lfs-eu-1.hf.co"
                        | "cas-bridge.xethub.hf.co"
                        | "us.aws.cdn.hf.co"
                        | "eu.aws.cdn.hf.co"
                )
            )
        {
            return Err(format!(
                "Disallowed recall artifact redirect: {}",
                artifact.name
            ));
        }
        // No implicit redirects: each hop is routed by this same egress-aware client.
        let mut request = Request::new(Method::GET, url.to_string());
        request.headers.insert(
            header::ACCEPT_ENCODING,
            rama::http::HeaderValue::from_static("identity"),
        );
        let mut response = client.stream_raw(request).await.map_err(|_| failure())?;
        if matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            if hop == 5 {
                return Err(format!(
                    "Too many recall artifact redirects: {}",
                    artifact.name
                ));
            }
            let location = response
                .headers
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(failure)?;
            url = url.join(location).map_err(|_| failure())?;
            continue;
        }
        if response.status != StatusCode::OK {
            return Err(format!("{} (HTTP {})", failure(), response.status.as_u16()));
        }
        if let Some(length) = response.headers.get(header::CONTENT_LENGTH)
            && length
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                != Some(artifact.size)
        {
            return Err(format!(
                "Recall artifact {} has an invalid download size",
                artifact.name
            ));
        }
        let path = directory.join(artifact.name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("write {}: {e}", artifact.name))?;
        let mut received = 0_u64;
        while let Some(chunk) = response.bytes.next().await {
            let chunk = chunk.map_err(|_| failure())?;
            if chunk.len() as u64 > artifact.size - received {
                return Err(format!(
                    "Recall artifact {} exceeds its pinned size",
                    artifact.name
                ));
            }
            file.write_all(&chunk)
                .map_err(|e| format!("write {}: {e}", artifact.name))?;
            received += chunk.len() as u64;
            tokio::task::yield_now().await;
        }
        drop(file);
        return verify_file(&path, directory, artifact);
    }
    Err(failure())
}

fn ambient_proxy() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Own transport tasks and blocking I/O until the shared attempt fully drains.
pub(super) async fn isolated<F, Fut, T>(
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, String>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>>,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .map_err(|e| format!("create recall download runtime: {e}"))?;
        let result = runtime.block_on(async move {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err("Recall preparation cancelled".into()),
                result = operation() => result,
            }
        });
        // Normal drop stops async children and joins outstanding blocking I/O.
        // Never use shutdown_background: a later retry must not overlap writes.
        drop(runtime);
        result
    })
    .await
    .map_err(|_| "Recall artifact worker failed to finish".to_owned())?
}

fn verify(directory: &Path, root: &Path, artifacts: &[Artifact]) -> Result<(), String> {
    for artifact in artifacts {
        verify_file(&directory.join(artifact.name), root, artifact)?;
    }
    Ok(())
}

fn verify_file(path: &Path, root: &Path, artifact: &Artifact) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("resolve recall model root: {e}"))?;
    let target = path
        .canonicalize()
        .map_err(|e| format!("read {}: {e}", artifact.name))?;
    if !target.starts_with(&root) {
        return Err(format!(
            "Recall artifact {} escapes its model cache",
            artifact.name
        ));
    }
    let metadata =
        std::fs::metadata(&target).map_err(|e| format!("stat {}: {e}", artifact.name))?;
    if !metadata.is_file() || metadata.len() != artifact.size {
        return Err(format!(
            "Recall artifact {} has an invalid size/type",
            artifact.name
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Reject a raced final symlink/FIFO without blocking on open.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(target)
        .map_err(|e| format!("open {}: {e}", artifact.name))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("stat {}: {e}", artifact.name))?;
    if !metadata.is_file() || metadata.len() != artifact.size {
        return Err(format!(
            "Recall artifact {} has an invalid size/type",
            artifact.name
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut remaining = artifact.size;
    while remaining > 0 {
        let count = file
            .read(&mut buffer[..remaining.min(65_536) as usize])
            .map_err(|e| format!("hash {}: {e}", artifact.name))?;
        if count == 0 {
            return Err(format!("Recall artifact {} is truncated", artifact.name));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.read(&mut buffer[..1]).map_err(|e| e.to_string())? != 0 {
        return Err(format!(
            "Recall artifact {} grew during validation",
            artifact.name
        ));
    }
    let digest: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != artifact.sha256 {
        return Err(format!(
            "Recall artifact {} failed SHA-256 validation",
            artifact.name
        ));
    }
    Ok(())
}

fn create_cache_directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("Recall cache must use real directories".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(|e| format!("create recall cache: {e}"))
        }
        Err(error) => Err(format!("inspect recall cache: {error}")),
    }
}

#[cfg(test)]
#[path = "assets/tests.rs"]
mod tests;
