use super::*;
use rama::http::Method;
use rama::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use serde_json::json;

#[tokio::test]
async fn request_compression_preserves_json_and_headers()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for json in [
        json!(null),
        json!({"input": "héllo 世界\n<&>", "count": 42}),
    ] {
        for compression in [RequestCompression::None, RequestCompression::Zstd] {
            let request = Request::new(Method::POST, "https://example.invalid/test".to_owned())
                .with_json(json.clone())
                .with_compression(compression);
            let request = RamaTransport::build_request(request)?;
            assert_eq!(request.headers()[CONTENT_TYPE], "application/json");
            match compression {
                RequestCompression::None => {
                    assert!(!request.headers().contains_key(CONTENT_ENCODING));
                }
                RequestCompression::Zstd => {
                    assert_eq!(request.headers()[CONTENT_ENCODING], "zstd");
                }
            }
            let body = request.into_body().collect().await?.to_bytes();
            let decoded = match compression {
                RequestCompression::None => body.to_vec(),
                RequestCompression::Zstd => zstd::stream::decode_all(body.as_ref())?,
            };
            assert_eq!(decoded, serde_json::to_vec(&json)?);
        }
    }
    Ok(())
}
