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
        let service = InfrastructureCookieLayer::with_store(Arc::clone(&store)).into_layer(inner);
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

#[tokio::test]
async fn egress_keeps_infrastructure_cookies_scoped_to_vendor_origins() {
    let store = Arc::new(Mutex::new(CookieStore::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let inner = service_fn(move |request: Request<Body>| {
        let captured = Arc::clone(&captured);
        async move {
            assert!(
                request
                    .uri()
                    .to_string()
                    .starts_with("https://gateway/egress/chaos/")
            );
            lock_requests(&captured).push(
                request
                    .headers()
                    .get(COOKIE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            );
            Ok::<_, OpaqueError>(
                Response::builder()
                    .header(SET_COOKIE, "__cf_bm=edge; Path=/; Secure")
                    .body(Body::empty())
                    .unwrap(),
            )
        }
    })
    .boxed();
    let service =
        InfrastructureCookieLayer::with_store(store).into_layer(crate::egress::with_egress(
            inner,
            Some(crate::Egress::parse("https://gateway/egress/chaos").unwrap()),
        ));
    for origin in ["first.example", "second.example", "first.example"] {
        service
            .serve(
                Request::builder()
                    .uri(format!("https://{origin}/v1/models"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        *lock_requests(&requests),
        vec![None, None, Some("__cf_bm=edge".into())]
    );
}
