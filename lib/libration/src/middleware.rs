//! `RationLayer` — rama middleware that sniffs rate-limit headers off
//! provider responses and records them in the [`UsageStore`].
//!
//! The middleware is transparent to the wrapped client: it forwards the
//! request unchanged, extracts windows from the response headers using a
//! provider-specific [`HeaderExtractor`], and fires the persistence
//! off in the background so the HTTP hot path never waits on the database.

use crate::UsageStore;
use chaos_ration::HeaderExtractor;
use rama::Layer;
use rama::Service;
use rama::http::Request;
use rama::http::Response;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Wrap a rama HTTP client so every response's rate-limit headers flow
/// into the usage store.
#[derive(Clone)]
pub struct RationLayer<E> {
    extractor: Arc<E>,
    store: Arc<UsageStore>,
}

impl<E> RationLayer<E> {
    /// Build a layer from a concrete header extractor and a usage store.
    pub fn new(extractor: E, store: Arc<UsageStore>) -> Self {
        Self {
            extractor: Arc::new(extractor),
            store,
        }
    }
}

impl<S, E> Layer<S> for RationLayer<E> {
    type Service = RationService<S, E>;

    fn layer(&self, inner: S) -> Self::Service {
        RationService {
            inner,
            extractor: Arc::clone(&self.extractor),
            store: Arc::clone(&self.store),
        }
    }
}

/// Service wrapper produced by [`RationLayer`].
#[derive(Clone)]
pub struct RationService<S, E> {
    inner: S,
    extractor: Arc<E>,
    store: Arc<UsageStore>,
}

impl<S, E, ReqBody, ResBody> Service<Request<ReqBody>> for RationService<S, E>
where
    S: Service<Request<ReqBody>, Output = Response<ResBody>>,
    E: HeaderExtractor,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Output = Response<ResBody>;
    type Error = S::Error;

    async fn serve(&self, req: Request<ReqBody>) -> Result<Self::Output, Self::Error> {
        let res = self.inner.serve(req).await?;

        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let windows = self.extractor.extract(res.headers(), observed_at);

        if !windows.is_empty() {
            let provider = self.extractor.provider().to_string();
            let store = Arc::clone(&self.store);
            tokio::spawn(async move {
                if let Err(err) = store.record(&provider, &windows).await {
                    tracing::warn!(target: "ration", %provider, %err, "failed to record usage snapshot");
                }
            });
        }

        Ok(res)
    }
}

/// Type-erased pairing of an extractor with a usage store, suitable for
/// stashing in an `Arc` and threading through transport code that isn't
/// structured as a rama `Service` stack. Construct one per provider at
/// boot and pass it by reference into the request path.
pub struct UsageSniffer {
    extractor: Box<dyn HeaderExtractor>,
    store: Arc<UsageStore>,
}

impl UsageSniffer {
    pub fn new<E>(extractor: E, store: Arc<UsageStore>) -> Self
    where
        E: HeaderExtractor + 'static,
    {
        Self {
            extractor: Box::new(extractor),
            store,
        }
    }

    /// Extract windows from `headers` and persist them in the background.
    pub fn sniff(&self, headers: &rama::http::HeaderMap) {
        sniff_and_record(self.extractor.as_ref(), &self.store, headers);
    }
}

/// Record rate-limit headers inline, without wrapping the HTTP client in a
/// rama `Service`. This is the hook-point for transports that assemble
/// requests imperatively (retry loops, manual SSE plumbing) where
/// slotting in a full middleware stack would be disruptive.
///
/// The actual write fires in the background via `tokio::spawn`, matching
/// [`RationService`]'s semantics: persistence never blocks the caller and
/// failures are logged through `tracing`.
pub fn sniff_and_record<E>(extractor: &E, store: &Arc<UsageStore>, headers: &rama::http::HeaderMap)
where
    E: HeaderExtractor + ?Sized,
{
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let windows = extractor.extract(headers, observed_at);
    if windows.is_empty() {
        return;
    }
    let provider = extractor.provider().to_string();
    let store = Arc::clone(store);
    tokio::spawn(async move {
        if let Err(err) = store.record(&provider, &windows).await {
            tracing::warn!(target: "ration", %provider, %err, "failed to record usage snapshot");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ration::UsageWindow;
    use chaos_storage::ChaosStorageProvider;
    use rama::http::Body;
    use rama::http::HeaderMap;
    use std::convert::Infallible;
    use std::sync::Mutex;

    /// Hardcoded extractor: returns a single "tokens" window with a known
    /// observed_at regardless of headers, so we can assert the middleware
    /// shuttled the response untouched and triggered the background write.
    struct FakeExtractor {
        calls: Arc<Mutex<usize>>,
    }

    impl HeaderExtractor for FakeExtractor {
        fn provider(&self) -> &str {
            "fake"
        }
        fn extract(&self, _headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow> {
            *self.calls.lock().unwrap() += 1;
            vec![UsageWindow::from_raw(
                "tokens",
                40_000,
                34_000,
                Some(observed_at + 3_600),
                observed_at,
            )]
        }
    }

    #[derive(Clone)]
    struct EchoOk;

    impl Service<Request<Body>> for EchoOk {
        type Output = Response<Body>;
        type Error = Infallible;

        async fn serve(&self, _req: Request<Body>) -> Result<Self::Output, Self::Error> {
            Ok(Response::builder()
                .status(200)
                .body(Body::empty())
                .expect("response"))
        }
    }

    #[tokio::test]
    async fn middleware_extracts_and_records_in_background() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = chaos_proc::open_runtime_db(dir.path())
            .await
            .expect("open runtime db");
        let provider = ChaosStorageProvider::from_sqlite_pool(pool);
        let store = Arc::new(UsageStore::from_provider(&provider).expect("store"));

        let calls = Arc::new(Mutex::new(0usize));
        let extractor = FakeExtractor {
            calls: Arc::clone(&calls),
        };
        let layer = RationLayer::new(extractor, Arc::clone(&store));
        let svc = layer.layer(EchoOk);

        let req = Request::builder()
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("request");
        let res = svc.serve(req).await.expect("serve");
        assert_eq!(res.status(), 200);
        assert_eq!(*calls.lock().unwrap(), 1, "extractor ran exactly once");

        // Give the fire-and-forget task a tick to commit.
        for _ in 0..50 {
            let rows = store.latest_all(0).await.expect("latest");
            if !rows.is_empty() {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].provider, "fake");
                assert_eq!(rows[0].window.remaining_percent(), 85);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("background recording never landed");
    }
}
