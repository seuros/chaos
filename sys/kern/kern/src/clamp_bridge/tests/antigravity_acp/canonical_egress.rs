//! Unconfined compatibility probe: prove effective canonical request replacement,
//! not that an agent is unable to bypass the proxy. Sandbox parity is separate.
use super::*;
use chaos_clamp::{EgressPolicy, EgressProxy, WiretapExchange, WiretapSink};

#[derive(Default)]
struct Observations {
    canonical: String,
    exchanges: AtomicUsize,
    generations: AtomicUsize,
    replaced: AtomicUsize,
    mismatched: AtomicUsize,
    denied: AtomicUsize,
    status_classes: Mutex<std::collections::BTreeMap<&'static str, usize>>,
}

impl WiretapSink for Observations {
    fn record(&self, exchange: WiretapExchange) {
        // Retain counters only. Never store headers, OAuth/control payloads,
        // provider messages, URLs or raw wiretap exchanges.
        self.exchanges.fetch_add(1, Ordering::Relaxed);
        let status_class = match exchange.status {
            Some(200..=299) => "http_2xx",
            Some(300..=399) => "http_3xx",
            Some(400..=499) => "http_4xx",
            Some(500..=599) => "http_5xx",
            Some(_) => "http_other",
            None => "upstream_transport_failure",
        };
        *self
            .status_classes
            .lock()
            .expect("observation counters")
            .entry(status_class)
            .or_default() += 1;
        if exchange.status == Some(403) {
            self.denied.fetch_add(1, Ordering::Relaxed);
        }
        if exchange.path.contains(":generateContent")
            || exchange.path.contains(":streamGenerateContent")
        {
            self.generations.fetch_add(1, Ordering::Relaxed);
            let effective = exchange
                .request
                .as_ref()
                .and_then(|request| {
                    request
                        .pointer("/request/systemInstruction/parts/0/text")
                        .or_else(|| request.pointer("/systemInstruction/parts/0/text"))
                })
                .and_then(Value::as_str);
            if effective == Some(self.canonical.as_str()) {
                self.replaced.fetch_add(1, Ordering::Relaxed);
            } else {
                self.mismatched.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

struct Report(Arc<Observations>);

impl Drop for Report {
    fn drop(&mut self) {
        eprintln!(
            "Canonical proxy observations: exchanges={}, generations={}, \
             replaced={}, mismatched={}, denied={}",
            self.0.exchanges.load(Ordering::Relaxed),
            self.0.generations.load(Ordering::Relaxed),
            self.0.replaced.load(Ordering::Relaxed),
            self.0.mismatched.load(Ordering::Relaxed),
            self.0.denied.load(Ordering::Relaxed),
        );
        eprintln!(
            "Canonical proxy status classes: {:?}",
            self.0.status_classes.lock().expect("observation counters")
        );
    }
}

#[ignore = "live canonical-egress compatibility probe; dedicated OAuth profile \
            and explicit ACP/hook paths required"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_agy_uses_chaos_egress_and_canonical_system_prompt() {
    let server = path_env("CHAOS_AGY_ACP_PATH");
    let home = path_env("CHAOS_AGY_ACP_HOME");
    let executable = path_env("CHAOS_AGY_ACP_HOOK_PATH");
    let _hook = Hook::deny_all(&home, &executable);
    let expected = format!("CANONICAL_{}", marker());
    let canonical = format!(
        "You are a Chaos integration test. Do not use tools. For every user message, \
         respond with exactly {expected} and nothing else."
    );
    let observations = Arc::new(Observations {
        canonical: canonical.clone(),
        ..Default::default()
    });
    let _report = Report(observations.clone());
    let certificates = tempfile::tempdir().expect("temporary CA directory");
    let proxy = EgressProxy::start(
        EgressPolicy::antigravity().with_antigravity_system_prompt(canonical),
        observations.clone(),
        Some(certificates.path().join("session-ca.pem")),
    )
    .await
    .expect("start existing Chaos egress proxy");
    let mut agent = process::Agent::spawn_proxied(
        &server,
        &home,
        &proxy.proxy_url(),
        proxy.ca_bundle_path().expect("CA path"),
    )
    .expect("proxied official ACP runtime");
    let (connection, mut updates) = required(
        AcpConnection::connect(
            agent.child.stdout.take().expect("stdout"),
            agent.child.stdin.take().expect("stdin"),
            ConnectionOptions {
                request_timeout: Duration::from_secs(120),
                ..Default::default()
            },
        )
        .await,
        "initialize proxied ACP",
    );
    let workspace = tempfile::tempdir().expect("empty workspace");
    let client = connection.client();
    let created = draining(
        &client,
        &mut updates,
        NewSessionRequest::new(workspace.path()),
    )
    .await;
    if created.is_err() {
        eprintln!(
            "Proxied startup diagnostic categories: {:?}",
            agent.diagnostic_categories()
        );
        if let Err(GuestError::Protocol(error)) = &created {
            // Classify in memory only; raw ACP messages/data may contain secrets.
            let categories = process::diagnostic_categories_for_text(&format!("{error:?}"));
            eprintln!("Proxied ACP error diagnostic categories: {categories:?}");
        }
    }
    let session = required(created, "session/new through Chaos egress");
    let output = prompt(
        &client,
        &mut updates,
        &session.session_id,
        "Reply with exactly USER_REQUESTED_OUTPUT and do not use any tools.".into(),
    )
    .await;
    eprintln!(
        "Proxied runtime diagnostic categories: {:?}",
        agent.diagnostic_categories()
    );
    connection
        .disconnect()
        .await
        .expect("disconnect proxied ACP");
    agent.stop().await.expect("reap proxied runtime group");
    proxy.shutdown();
    assert!(
        output.trim() == expected,
        "canonical instructions did not control the live response"
    );
    assert!(
        observations.replaced.load(Ordering::Relaxed) > 0,
        "no effective generation request with canonical instructions observed; \
         proxy parity unproven"
    );
    assert_eq!(observations.mismatched.load(Ordering::Relaxed), 0);
}
