use cookie_store::CookieStore;
use cookie_store::RawCookie;
use rama::Layer;
use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::HeaderMap;
use rama::http::HeaderValue;
use rama::http::Request;
use rama::http::Response;
use rama::http::header::COOKIE;
use rama::http::header::SET_COOKIE;
use rama::service::BoxService;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use url::Url;

type SharedCookieStore = Arc<Mutex<CookieStore>>;

// WARNING: this store is process-global and shared across providers and auth contexts.
// It must only contain infrastructure cookies. Never extend the allowlist with account,
// session, authorization, or other user-specific cookies.
static SHARED_INFRASTRUCTURE_COOKIE_STORE: LazyLock<SharedCookieStore> =
    LazyLock::new(|| Arc::new(Mutex::new(CookieStore::new())));

/// Rama layer that persists a small allowlist of CDN/WAF cookies across outbound requests.
///
/// The layer is intentionally always-on at the shared HTTP-client boundary. Cookies are
/// accepted and replayed only for HTTPS URLs, forced to the exact response host, and filtered
/// by name so a process-global jar cannot retain provider authentication or session state.
#[derive(Clone, Debug)]
pub struct InfrastructureCookieLayer {
    store: SharedCookieStore,
}

impl Default for InfrastructureCookieLayer {
    fn default() -> Self {
        Self {
            store: Arc::clone(&SHARED_INFRASTRUCTURE_COOKIE_STORE),
        }
    }
}

impl InfrastructureCookieLayer {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_store(store: SharedCookieStore) -> Self {
        Self { store }
    }
}

impl<S> Layer<S> for InfrastructureCookieLayer {
    type Service = InfrastructureCookieService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InfrastructureCookieService {
            inner,
            store: Arc::clone(&self.store),
        }
    }

    fn into_layer(self, inner: S) -> Self::Service {
        InfrastructureCookieService {
            inner,
            store: self.store,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InfrastructureCookieService<S> {
    inner: S,
    store: SharedCookieStore,
}

impl<ReqBody, ResBody, S> Service<Request<ReqBody>> for InfrastructureCookieService<S>
where
    S: Service<Request<ReqBody>, Output = Response<ResBody>>,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Output = S::Output;
    type Error = S::Error;

    async fn serve(&self, mut request: Request<ReqBody>) -> Result<Self::Output, Self::Error> {
        let url = secure_absolute_url(request.uri());
        if let Some(url) = url.as_ref() {
            inject_request_cookies(request.headers_mut(), url, &self.store);
        }

        let response = self.inner.serve(request).await?;

        if let Some(url) = url.as_ref() {
            retain_response_cookies(response.headers(), url, &self.store);
        }

        Ok(response)
    }
}

pub(crate) fn with_infrastructure_cookies(
    client: BoxService<rama::http::Request, rama::http::Response, OpaqueError>,
) -> BoxService<rama::http::Request, rama::http::Response, OpaqueError> {
    InfrastructureCookieLayer::new().into_layer(client).boxed()
}

pub fn default_rama_http_client()
-> BoxService<rama::http::Request, rama::http::Response, OpaqueError> {
    crate::ensure_rustls_crypto_provider();
    InfrastructureCookieLayer::new()
        .into_layer(rama::http::client::EasyHttpWebClient::default())
        .boxed()
}

fn secure_absolute_url(uri: &impl std::fmt::Display) -> Option<Url> {
    let url = Url::parse(&uri.to_string()).ok()?;
    (url.scheme() == "https" && url.host_str().is_some()).then_some(url)
}

fn inject_request_cookies(headers: &mut HeaderMap, url: &Url, store: &SharedCookieStore) {
    let stored = lock_store(store)
        .get_request_values(url)
        .filter(|(name, _)| is_allowed_infrastructure_cookie_name(name))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();
    if stored.is_empty() {
        return;
    }

    let mut pairs = Vec::new();
    let mut existing_names = HashSet::new();
    for header in headers.get_all(COOKIE).iter() {
        let Ok(value) = header.to_str() else {
            // Do not replace a caller-provided cookie header we cannot safely merge.
            return;
        };
        for pair in value
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            if let Some(name) = cookie_pair_name(pair) {
                existing_names.insert(name.to_owned());
            }
            pairs.push(pair.to_owned());
        }
    }

    for (name, value) in stored {
        if existing_names.insert(name.clone()) {
            pairs.push(format!("{name}={value}"));
        }
    }

    let Ok(mut value) = HeaderValue::from_str(&pairs.join("; ")) else {
        return;
    };
    value.set_sensitive(true);
    headers.remove(COOKIE);
    headers.insert(COOKIE, value);
}

fn retain_response_cookies(headers: &HeaderMap, url: &Url, store: &SharedCookieStore) {
    let cookies = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| RawCookie::parse(value.to_owned()).ok())
        .filter(|cookie| is_allowed_infrastructure_cookie_name(cookie.name()))
        .map(|mut cookie| {
            // A process-global jar must not let one provider set a parent-domain cookie
            // that could be replayed to another provider host.
            cookie.unset_domain();
            cookie
        })
        .collect::<Vec<_>>();

    if !cookies.is_empty() {
        lock_store(store).store_response_cookies(cookies.into_iter(), url);
    }
}

fn lock_store(store: &SharedCookieStore) -> std::sync::MutexGuard<'_, CookieStore> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cookie_pair_name(pair: &str) -> Option<&str> {
    let (name, _) = pair.split_once('=')?;
    let name = name.trim();
    (!name.is_empty()).then_some(name)
}

fn is_allowed_infrastructure_cookie_name(name: &str) -> bool {
    matches!(
        name,
        // Cloudflare service cookies.
        "__cf_bm"
            | "__cflb"
            | "__cfruid"
            | "__cfseq"
            | "__cfwaitingroom"
            | "_cfuvid"
            | "cf_clearance"
            | "cf_ob_info"
            | "cf_use_ob"
            // Alibaba Cloud Global Accelerator / WAF cookies observed on Z.ai.
            | "acw_tc"
            | "acw_sc__v2"
            | "acw_sc__v3"
    ) || name.starts_with("cf_chl_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rama::http::Body;
    use rama::service::service_fn;
    use std::convert::Infallible;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[test]
    fn allowlist_excludes_provider_session_cookies() {
        assert!(is_allowed_infrastructure_cookie_name("__cf_bm"));
        assert!(is_allowed_infrastructure_cookie_name("cf_chl_2"));
        assert!(is_allowed_infrastructure_cookie_name("acw_tc"));
        assert!(!is_allowed_infrastructure_cookie_name("session"));
        assert!(!is_allowed_infrastructure_cookie_name(
            "__Secure-next-auth.session-token"
        ));
    }

    #[test]
    fn cookies_are_https_host_and_path_scoped() {
        let store = Arc::new(Mutex::new(CookieStore::new()));
        let url = Url::parse("https://api.example.com/v1/responses").unwrap();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("__cflb=west; Path=/v1; Secure; HttpOnly"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("session=secret; Path=/; Secure; HttpOnly"),
        );
        retain_response_cookies(&headers, &url, &store);

        let mut matching = HeaderMap::new();
        inject_request_cookies(&mut matching, &url, &store);
        assert_eq!(
            matching.get(COOKIE).and_then(|value| value.to_str().ok()),
            Some("__cflb=west")
        );

        let mut wrong_path = HeaderMap::new();
        inject_request_cookies(
            &mut wrong_path,
            &Url::parse("https://api.example.com/v2/models").unwrap(),
            &store,
        );
        assert!(wrong_path.get(COOKIE).is_none());

        let mut sibling_host = HeaderMap::new();
        inject_request_cookies(
            &mut sibling_host,
            &Url::parse("https://other.example.com/v1/responses").unwrap(),
            &store,
        );
        assert!(sibling_host.get(COOKIE).is_none());
    }

    #[test]
    fn explicit_request_cookie_wins_over_stored_cookie() {
        let store = Arc::new(Mutex::new(CookieStore::new()));
        let url = Url::parse("https://api.example.com/v1/responses").unwrap();
        lock_store(&store).store_response_cookies(
            std::iter::once(RawCookie::parse("__cf_bm=stored; Path=/; Secure".to_owned()).unwrap()),
            &url,
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("__cf_bm=caller; custom=value"),
        );
        inject_request_cookies(&mut headers, &url, &store);

        assert_eq!(
            headers.get(COOKIE).and_then(|value| value.to_str().ok()),
            Some("__cf_bm=caller; custom=value")
        );
    }

    #[tokio::test]
    async fn layer_shares_edge_cookie_across_fresh_services() {
        let store = Arc::new(Mutex::new(CookieStore::new()));
        let requests = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let call = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let requests = Arc::clone(&requests);
            let call = Arc::clone(&call);
            let inner = service_fn(move |request: Request<Body>| {
                let requests = Arc::clone(&requests);
                let call = Arc::clone(&call);
                async move {
                    lock_requests(&requests).push(
                        request
                            .headers()
                            .get(COOKIE)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    );
                    let index = call.fetch_add(1, Ordering::SeqCst);
                    let mut response = Response::builder();
                    if index == 0 {
                        response = response
                            .header(SET_COOKIE, "__cf_bm=edge; Path=/; Secure")
                            .header(SET_COOKIE, "account_session=secret; Path=/; Secure");
                    }
                    Ok::<_, Infallible>(response.body(Body::empty()).unwrap())
                }
            });
            let service =
                InfrastructureCookieLayer::with_store(Arc::clone(&store)).into_layer(inner);
            let request = Request::builder()
                .uri("https://api.example.com/v1/responses")
                .body(Body::empty())
                .unwrap();
            service.serve(request).await.unwrap();
        }

        assert_eq!(
            *lock_requests(&requests),
            vec![None, Some("__cf_bm=edge".to_owned())]
        );
    }

    fn lock_requests(
        requests: &Arc<Mutex<Vec<Option<String>>>>,
    ) -> std::sync::MutexGuard<'_, Vec<Option<String>>> {
        requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
