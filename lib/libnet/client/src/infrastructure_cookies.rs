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
mod tests;
