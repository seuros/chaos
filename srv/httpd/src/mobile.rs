//! The LAN listener deliberately does not expose the execution or MCP router.
use crate::{ServeCli, api::json_response, auth, ingress};
use anyhow::{Context, Result, ensure};
use chaos_kern::activity::fleet::relay;
use qrcode::{QrCode, render::svg};
use rama::{
    Layer,
    crypto::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    http::server::HttpServer,
    http::{Body, HeaderValue, Method, Request, Response, StatusCode},
    rt::Executor,
    service::service_fn,
    tcp::server::TcpListener,
    tls::{
        rustls::server::TlsAcceptorLayer,
        server::{ServerAuthData, TlsServerConfig},
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use uuid::Uuid;

mod pairing;

pub(crate) struct Relay {
    pool: PgPool,
    endpoint: String,
    pin: String,
    admin: String,
}

pub(crate) async fn prepare(
    cli: &ServeCli,
    storage: &str,
) -> Result<Option<(Arc<Relay>, SocketAddr, TlsServerConfig)>> {
    let Some(addr) = cli.mobile_listen else {
        ensure!(
            cli.mobile_admin_file.is_none()
                && cli.mobile_cert.is_none()
                && cli.mobile_key.is_none(),
            "mobile files require --mobile-listen"
        );
        return Ok(None);
    };
    ensure!(
        cli.bind.parse::<std::net::IpAddr>()?.is_loopback()
            && !addr.ip().is_unspecified()
            && !addr.ip().is_multicast()
            && addr.port() != 0,
        "use a loopback web monitor and a concrete phone listener address"
    );
    ensure!(
        chaos_kern::activity::fleet::database::shared(storage),
        "mobile needs PostgreSQL"
    );
    let secret = std::fs::read_to_string(
        cli.mobile_admin_file
            .as_ref()
            .context("missing --mobile-admin-file")?,
    )?;
    let admin = secret.trim();
    ensure!(
        admin.len() >= 32
            && [
                cli.bearer_token.as_deref(),
                cli.monitor_token.as_deref(),
                cli.recovery_token.as_deref()
            ]
            .into_iter()
            .flatten()
            .all(|s| s != admin),
        "use a distinct random mobile operator secret"
    );
    #[cfg(feature = "mcpd")]
    ensure!(
        cli.operator_token.as_deref() != Some(admin),
        "do not reuse the MCP operator key"
    );
    let cert =
        CertificateDer::from_pem_file(cli.mobile_cert.as_ref().context("missing --mobile-cert")?)?;
    let key =
        PrivateKeyDer::from_pem_file(cli.mobile_key.as_ref().context("missing --mobile-key")?)?;
    let pin = digest(cert.as_ref());
    let tls = TlsServerConfig::new().with_single_cert(ServerAuthData::new(vec![cert], key));
    let pool = PgPool::connect(storage).await?;
    relay::initialize(&pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS chaos_mobile_keys (pin text NOT NULL, hash text PRIMARY KEY, invite boolean NOT NULL, expires timestamptz NOT NULL)").execute(&pool).await?;
    sqlx::query("ALTER TABLE chaos_mobile_keys ADD COLUMN IF NOT EXISTS device_id uuid")
        .execute(&pool)
        .await?;
    sqlx::query("ALTER TABLE chaos_mobile_keys ADD COLUMN IF NOT EXISTS pair_lookup text, ADD COLUMN IF NOT EXISTS pair_proof text")
        .execute(&pool).await?;
    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS chaos_mobile_invite_lookup ON chaos_mobile_keys(pair_lookup) WHERE pair_lookup IS NOT NULL")
        .execute(&pool).await?;
    sqlx::query("DELETE FROM chaos_mobile_keys WHERE expires <= now()")
        .execute(&pool)
        .await?;
    Ok(Some((
        Arc::new(Relay {
            pool,
            endpoint: format!("https://{addr}"),
            pin,
            admin: admin.into(),
        }),
        addr,
        tls,
    )))
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn random() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn text(content: impl Into<Body>, mime: &'static str) -> Response {
    let mut r = Response::new(content.into());
    r.headers_mut()
        .insert("content-type", HeaderValue::from_static(mime));
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r.headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    r.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    r
}
fn status(code: StatusCode) -> Response {
    json_response(code, &json!({"status":code.as_u16()}))
}
fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub(crate) async fn admin(relay: &Relay, req: Request) -> Response {
    let path = req
        .uri()
        .path()
        .map(|p| p.as_encoded_str())
        .unwrap_or_default();
    if path == "/monitor/phone" && req.method() == Method::GET {
        return text(
            include_str!("mobile/operator.html"),
            "text/html; charset=utf-8",
        );
    }
    if req.method() != Method::POST
        || auth::validate_bearer(
            req.headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok()),
            &relay.admin,
        )
        .is_err()
    {
        return status(StatusCode::UNAUTHORIZED);
    }
    match path.as_ref() {
        "/monitor/phone/devices" => match sqlx::query_scalar::<_,Uuid>("SELECT device_id FROM chaos_mobile_keys WHERE pin=$1 AND device_id IS NOT NULL AND expires>now() ORDER BY expires DESC").bind(&relay.pin).fetch_all(&relay.pool).await {
            Ok(devices) => json_response(StatusCode::OK, &devices),
            Err(_) => status(StatusCode::SERVICE_UNAVAILABLE),
        },
        p if p.starts_with("/monitor/phone/revoke/") => {
            let Ok(id) = Uuid::parse_str(&p["/monitor/phone/revoke/".len()..]) else {return status(StatusCode::BAD_REQUEST)};
            match sqlx::query("DELETE FROM chaos_mobile_keys WHERE pin=$1 AND device_id=$2").bind(&relay.pin).bind(id).execute(&relay.pool).await {
                Ok(_) => status(StatusCode::OK),Err(_) => status(StatusCode::SERVICE_UNAVAILABLE)
            }
        }
        "/monitor/phone/revoke" => match sqlx::query("DELETE FROM chaos_mobile_keys WHERE pin=$1")
            .bind(&relay.pin)
            .execute(&relay.pool)
            .await
        {
            Ok(_) => status(StatusCode::OK),
            Err(_) => status(StatusCode::SERVICE_UNAVAILABLE),
        },
        "/monitor/phone/invite" => {
            let code = pairing::generate();
            let token = pairing::tag(&code, "invite", &relay.pin);
            let payload =
                json!({"v":1,"endpoint":relay.endpoint,"sha256":relay.pin,"invite":token});
            let Ok(qr) = QrCode::new(payload.to_string()) else {
                return status(StatusCode::INTERNAL_SERVER_ERROR);
            };
            if sqlx::query(
                "INSERT INTO chaos_mobile_keys(pin,hash,pair_lookup,pair_proof,invite,expires) VALUES ($1,$2,$3,$4,true,now()+interval '2 minutes')",
            )
            .bind(&relay.pin)
            .bind(digest(token.as_bytes()))
            .bind(pairing::tag(&code, "lookup", ""))
            .bind(pairing::tag(&code, "pin", &relay.pin))
            .execute(&relay.pool)
            .await
            .is_err()
            {
                return status(StatusCode::SERVICE_UNAVAILABLE);
            }
            let svg = qr.render::<svg::Color>().min_dimensions(320, 320).build();
            let address = relay.endpoint.trim_start_matches("https://");
            let groups = (0..5).map(|i| &code[i*4..i*4+4]).collect::<Vec<_>>().join("-");
            let manual = format!("{address}/{groups}");
            match req.headers().get("accept").and_then(|h| h.to_str().ok()) {
                Some("application/json") => text(json!({"code":manual,"qr":svg}).to_string(), "application/json"),
                Some("text/plain") => text(manual, "text/plain; charset=utf-8"),
                _ => text(svg, "image/svg+xml"),
            }
        }
        _ => status(StatusCode::NOT_FOUND),
    }
}

pub(crate) async fn listen(
    relay: Arc<Relay>,
    addr: SocketAddr,
    tls: TlsServerConfig,
    exec: Executor,
) -> Result<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::build(exec.clone())
        .bind_address(addr)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let service = HttpServer::new_http1(exec).service(service_fn(move |request: Request| {
        let relay = relay.clone();
        async move {
            let mut result = phone(&relay, request)
                .await
                .unwrap_or_else(|_| status(StatusCode::SERVICE_UNAVAILABLE));
            result
                .headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            Ok::<_, Infallible>(result)
        }
    }));
    Ok(tokio::spawn(async move {
        listener
            .serve(TlsAcceptorLayer::new(tls).into_layer(service))
            .await;
    }))
}

async fn phone(relay: &Relay, req: Request) -> Result<Response> {
    let path = req
        .uri()
        .path()
        .map(|p| p.as_encoded_str().to_string())
        .unwrap_or_default();
    // Deliberately credential-free: no invite token, code, device or fleet
    // data is disclosed. Never place the lookup identifier in URLs/access logs.
    if req.method() == Method::GET && path == "/v1/pair-info" {
        let Some(lookup) = req
            .headers()
            .get("x-chaos-pair-lookup")
            .and_then(|h| h.to_str().ok())
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return Ok(status(StatusCode::NOT_FOUND));
        };
        let proof: Option<String> = sqlx::query_scalar("SELECT pair_proof FROM chaos_mobile_keys WHERE pin=$1 AND pair_lookup=$2 AND invite AND expires>now()")
            .bind(&relay.pin).bind(lookup).fetch_optional(&relay.pool).await?;
        return Ok(match proof {
            Some(proof) => {
                json_response(StatusCode::OK, &json!({"sha256":relay.pin, "proof":proof}))
            }
            None => status(StatusCode::NOT_FOUND),
        });
    }
    let Some(token) = bearer(&req).filter(|s| s.len() == 64) else {
        return Ok(status(StatusCode::UNAUTHORIZED));
    };
    let hashed = digest(token.as_bytes());
    if req.method() == Method::POST && path == "/v1/pair" {
        let mut tx = relay.pool.begin().await?;
        let claimed = sqlx::query(
            "DELETE FROM chaos_mobile_keys WHERE pin=$1 AND hash=$2 AND invite AND expires>now()",
        )
        .bind(&relay.pin)
        .bind(hashed)
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
            return Ok(status(StatusCode::UNAUTHORIZED));
        }
        let device = random();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO chaos_mobile_keys(pin,hash,invite,expires,device_id) VALUES($1,$2,false,now()+interval '90 days',$3)")
            .bind(&relay.pin)
            .bind(digest(device.as_bytes()))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(json_response(
            StatusCode::OK,
            &json!({"device":device,"id":id}),
        ));
    }
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chaos_mobile_keys WHERE pin=$1 AND hash=$2 AND NOT invite AND expires>now())")
        .bind(&relay.pin).bind(hashed).fetch_one(&relay.pool).await?;
    if !allowed {
        return Ok(status(StatusCode::UNAUTHORIZED));
    }
    if req.method() == Method::GET
        && let Some(id) = path
            .strip_prefix("/v1/history/")
            .and_then(|s| Uuid::parse_str(s).ok())
    {
        return Ok(match relay::history(&relay.pool, id).await? {
            Some(entries) => json_response(StatusCode::OK, &entries),
            None => status(StatusCode::NOT_FOUND),
        });
    }
    if req.method() == Method::GET && path == "/v1/inbox" {
        return Ok(json_response(
            StatusCode::OK,
            &relay::inbox(&relay.pool).await?,
        ));
    }
    if req.method() == Method::POST
        && let Some(id) = path
            .strip_prefix("/v1/answer/")
            .and_then(|s| Uuid::parse_str(s).ok())
    {
        let limit = 16384;
        if let Err(e) = ingress::validate_json_headers(&req, limit) {
            return Ok(status(e.status()));
        }
        let scope = chaos_kern::TurnScope::after(Duration::from_secs(5));
        let body = match ingress::read_body(req.into_body(), limit, &scope).await {
            Ok(v) => v,
            Err(e) => return Ok(status(e.status())),
        };
        let Ok(answer) = serde_json::from_slice(&body) else {
            return Ok(status(StatusCode::BAD_REQUEST));
        };
        return Ok(status(if relay::propose(&relay.pool, id, answer).await? {
            StatusCode::ACCEPTED
        } else {
            StatusCode::BAD_REQUEST
        }));
    }
    Ok(status(StatusCode::NOT_FOUND))
}

#[cfg(test)]
mod tests;
