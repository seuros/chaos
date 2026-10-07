use super::*;
use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::{Body, Response};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn download_and_cdn_redirect_both_use_configured_egress() {
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let gateway = rama::service::service_fn(move |request: rama::http::Request| {
        let hop = observed.fetch_add(1, Ordering::SeqCst);
        async move {
            assert_eq!(request.method(), Method::GET);
            let response = if hop == 0 {
                assert_eq!(
                    request.uri().to_string(),
                    format!("http://gateway/egress/chaos/{MODEL}/resolve/{REVISION}/test")
                );
                assert_eq!(
                    request.headers()[chaos_client::EGRESS_UPSTREAM_HEADER],
                    "https://huggingface.co"
                );
                Response::builder()
                    .status(307)
                    .header(
                        header::LOCATION,
                        "https://cdn-lfs.hf.co/file?signature=opaque",
                    )
                    .body(Body::empty())
            } else {
                assert_eq!(hop, 1);
                assert_eq!(
                    request.uri().to_string(),
                    "http://gateway/egress/chaos/file?signature=opaque"
                );
                assert_eq!(
                    request.headers()[chaos_client::EGRESS_UPSTREAM_HEADER],
                    "https://cdn-lfs.hf.co"
                );
                Response::builder()
                    .header(header::CONTENT_LENGTH, "3")
                    .body(Body::from("abc"))
            };
            Ok::<_, OpaqueError>(response.unwrap())
        }
    })
    .boxed();
    let client = RamaTransport::new_with_egress(
        gateway,
        Some(Egress::parse("http://gateway/egress/chaos").unwrap()),
    );
    let directory = tempfile::tempdir().unwrap();
    download(
        &client,
        &Artifact {
            name: "test",
            size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        },
        directory.path(),
    )
    .await
    .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read(directory.path().join("test")).unwrap(),
        b"abc"
    );
}
