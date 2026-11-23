use super::*;
use rama::http::body::util::BodyExt;

#[derive(usage::Cli)]
#[usage(bin = "relay-test")]
struct TestCli {
    #[usage(flatten)]
    serve: ServeCli,
}

fn request(method: Method, path: &str, token: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn exchanging_retrying_expiring_and_revoking_are_scoped_to_a_device() {
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let key =
        rama::crypto::dep::rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = root.path().join("cert");
    let private = root.path().join("key");
    let admin_file = root.path().join("admin");
    std::fs::write(&cert, key.cert.pem()).unwrap();
    std::fs::write(&private, key.signing_key.serialize_pem()).unwrap();
    let operator = random();
    std::fs::write(&admin_file, &operator).unwrap();
    let args = [
        "--mobile-listen",
        "127.0.0.1:4443",
        "--mobile-cert",
        cert.to_str().unwrap(),
        "--mobile-key",
        private.to_str().unwrap(),
        "--mobile-admin-file",
        admin_file.to_str().unwrap(),
    ];
    let cli = TestCli::parse_from(&args.map(std::ffi::OsStr::new))
        .unwrap()
        .serve;
    let (relay, _, _) = prepare(&cli, &std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap()
        .unwrap();
    let invite = "/monitor/phone/invite";
    assert_eq!(
        admin(&relay, request(Method::POST, invite, "viewer"))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut issue = request(Method::POST, invite, &operator);
    issue
        .headers_mut()
        .insert("accept", HeaderValue::from_static("application/json"));
    let issued = admin(&relay, issue).await;
    assert_eq!(issued.status(), StatusCode::OK);
    assert_eq!(issued.headers()["cache-control"], "no-store");
    let issued: serde_json::Value =
        serde_json::from_slice(&issued.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(issued["qr"].as_str().unwrap().contains("<svg"));
    let code = issued["code"]
        .as_str()
        .unwrap()
        .strip_prefix("127.0.0.1:4443/")
        .unwrap()
        .replace('-', "");
    assert_eq!(code.len(), 20);
    let lookup = pairing::tag(&code, "lookup", "");
    let info = |lookup: &str| {
        phone(
            &relay,
            Request::builder()
                .uri("/v1/pair-info")
                .header("x-chaos-pair-lookup", lookup)
                .body(Body::empty())
                .unwrap(),
        )
    };
    assert_eq!(
        info(&random()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let proof = info(&lookup).await.unwrap();
    assert_eq!(proof.status(), StatusCode::OK);
    let proof: serde_json::Value =
        serde_json::from_slice(&proof.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        proof,
        json!({"sha256":relay.pin,"proof":pairing::tag(&code,"pin",&relay.pin)})
    );
    let token = pairing::tag(&code, "invite", &relay.pin);
    let pair = || phone(&relay, request(Method::POST, "/v1/pair", &token));
    let (a, b) = tokio::join!(pair(), pair());
    let (a, b) = (a.unwrap(), b.unwrap());
    let (winner, loser) = if a.status() == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(loser.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(info(&lookup).await.unwrap().status(), StatusCode::NOT_FOUND);
    let data: serde_json::Value =
        serde_json::from_slice(&winner.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let device = data["device"].as_str().unwrap();
    let id = data["id"].as_str().unwrap();
    let read = || phone(&relay, request(Method::GET, "/v1/inbox", device));
    assert_eq!(read().await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        admin(&relay, request(Method::POST, invite, device))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        phone(&relay, request(Method::POST, "/api/trigger", device))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin(
            &relay,
            request(
                Method::POST,
                &format!("/monitor/phone/revoke/{id}"),
                &operator
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(read().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    sqlx::query("INSERT INTO chaos_mobile_keys(pin,hash,invite,expires) VALUES($1,$2,true,now()-interval '1 second')")
        .bind(&relay.pin).bind(digest(token.as_bytes())).execute(&pool).await.unwrap();
    assert_eq!(pair().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    sqlx::query("DELETE FROM chaos_mobile_keys WHERE pin=$1")
        .bind(&relay.pin)
        .execute(&pool)
        .await
        .unwrap();
}
