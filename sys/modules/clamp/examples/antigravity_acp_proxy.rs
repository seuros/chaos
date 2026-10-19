//! Credential-redacting ACP proxy diagnostic, independent of the kernel.
//! Run with explicit official SERVER and dedicated PRIVATE_HOME arguments.
//! No model prompt unless CHAOS_AGY_ACP_CANONICAL_PROBE=1 is explicitly set.
//! Unconfined compatibility by default. CHAOS_AGY_ACP_SANDBOX_PROBE=1 uses
//! strict platform confinement and requires an explicit sandbox helper.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acp_guest::protocol::schema::v1::{
    ContentBlock, NewSessionRequest, PromptRequest, SessionUpdate, StopReason, TextContent,
};
use acp_guest::{AcpConnection, ConnectionOptions, GuestError};
use chaos_clamp::{EgressPolicy, EgressProxy, WiretapExchange, WiretapSink};

#[path = "../../../../lib/libacp/guest/examples/support/mod.rs"]
#[allow(dead_code)] // Shared examples select either direct or proxied spawning.
mod support;

#[path = "../../../../lib/libacp/guest/examples/support/hook.rs"]
#[allow(dead_code)] // The canonical probe uses deny-all, not the fixture allowlist.
mod hook;

#[derive(Default)]
struct Counters {
    values: Mutex<BTreeMap<&'static str, usize>>,
    canonical: String,
}

impl WiretapSink for Counters {
    fn record(&self, exchange: WiretapExchange) {
        let status = match exchange.status {
            Some(200..=299) => "http_2xx",
            Some(300..=399) => "http_3xx",
            Some(400..=499) => "http_4xx",
            Some(500..=599) => "http_5xx",
            Some(_) => "http_other",
            None => "upstream_transport_failure",
        };
        if let Ok(mut counters) = self.values.lock() {
            *counters.entry(status).or_default() += 1;
            if exchange.path.contains(":generateContent")
                || exchange.path.contains(":streamGenerateContent")
            {
                let effective = exchange
                    .request
                    .as_ref()
                    .and_then(|request| {
                        request
                            .pointer("/request/systemInstruction/parts/0/text")
                            .or_else(|| request.pointer("/systemInstruction/parts/0/text"))
                    })
                    .and_then(serde_json::Value::as_str);
                let label = if effective == Some(self.canonical.as_str()) {
                    "canonical_replaced"
                } else {
                    "canonical_mismatch"
                };
                *counters.entry(label).or_default() += 1;
            }
        }
        // Drop everything else, including OAuth payloads. Never print records.
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (server, home) = support::parameters()?;
    let canonical_probe = match std::env::var("CHAOS_AGY_ACP_CANONICAL_PROBE") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => true,
        _ => return Err("CHAOS_AGY_ACP_CANONICAL_PROBE must be absent or exactly 1".into()),
    };
    let sandbox_probe = match std::env::var("CHAOS_AGY_ACP_SANDBOX_PROBE") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => true,
        _ => return Err("CHAOS_AGY_ACP_SANDBOX_PROBE must be absent or exactly 1".into()),
    };
    let _hook = if canonical_probe
        || sandbox_probe
        || std::env::var_os("CHAOS_AGY_ACP_HOOK_PATH").is_some()
    {
        let executable = PathBuf::from(
            std::env::var_os("CHAOS_AGY_ACP_HOOK_PATH").ok_or("explicit hook path required")?,
        );
        if !executable.is_absolute() || !executable.is_file() {
            return Err("absolute compiled deny-tools hook required".into());
        }
        Some(hook::Hook::deny_all(&home, &executable))
    } else {
        None
    };
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let marker: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let expected = format!("CANONICAL_{marker}");
    let canonical = format!(
        "You are a Chaos integration test. Do not use tools. For every user message, \
         respond with exactly {expected} and nothing else."
    );
    let counters = Arc::new(Counters {
        canonical: canonical.clone(),
        ..Default::default()
    });
    let certificates = tempfile::tempdir()?;
    let proxy = EgressProxy::start(
        EgressPolicy::antigravity().with_antigravity_system_prompt(canonical),
        counters.clone(),
        Some(certificates.path().join("ca.pem")),
    )
    .await
    .map_err(|_| "could not start strict Chaos egress proxy")?;
    let ca = proxy.ca_bundle_path().ok_or("missing session CA")?;
    let control = tokio::process::Command::new("/usr/bin/curl")
        .env_clear()
        .args([
            "--silent",
            "--http1.1",
            "--max-time",
            "15",
            "--proxy",
            &proxy.proxy_url(),
            "--cacert",
        ])
        .arg(ca)
        .args([
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            "https://oauth2.googleapis.com/",
        ])
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    let status = std::str::from_utf8(&control.stdout)
        .ok()
        .filter(|status| status.len() == 3 && status.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap_or("invalid");
    eprintln!(
        "Credential-free proxy control: curl_exit={:?}, http_status={status}",
        control.status.code()
    );
    eprintln!(
        "Control status classes: {:?}",
        counters.values.lock().map_err(|_| "counter lock")?
    );
    if (canonical_probe || sandbox_probe) && (!control.status.success() || status != "404") {
        return Err(
            "credential-free proxy control failed; no ACP session or prompt submitted".into(),
        );
    }
    counters.values.lock().map_err(|_| "counter lock")?.clear();

    let workspace = tempfile::tempdir()?;
    let mut agent = if sandbox_probe {
        let helper = PathBuf::from(
            std::env::var_os("CHAOS_AGY_ACP_SANDBOX_HELPER")
                .ok_or("explicit platform sandbox helper required")?,
        );
        let sandbox = chaos_clamp::prepare_antigravity_sandbox(
            &helper,
            &home,
            workspace.path(),
            &chaos_clamp::AntigravityEgress {
                proxy_url: proxy.proxy_url(),
                ca_bundle_path: Some(ca.to_path_buf()),
            },
            &workspace.path().join("unused-bridge.sock"),
            &[home.join(".gemini/config")],
            &[
                home.join(".gemini/antigravity-acp"),
                home.join(".gemini/antigravity-cli"),
                home.join(".cache"),
                home.join(".config"),
                home.join(".chaos"),
            ],
        )
        .await
        .map_err(|_| "could not prepare strict platform confinement")?;
        support::Agent::spawn_wrapped_proxied(
            &server,
            &home,
            &proxy.proxy_url(),
            ca,
            &sandbox.program,
            &sandbox.args,
        )?
    } else {
        support::Agent::spawn_proxied(&server, &home, &proxy.proxy_url(), ca)?
    };
    let result =
        async {
            let (connection, mut updates) = AcpConnection::connect(
                agent.child.stdout.take().ok_or("missing stdout")?,
                agent.child.stdin.take().ok_or("missing stdin")?,
                ConnectionOptions {
                    request_timeout: Duration::from_secs(120),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| "ACP initialization failed")?;
            let observer = tokio::spawn(async move {
                let mut output = String::new();
                let mut overflow = false;
                while let Some(update) = updates.recv().await {
                    if let SessionUpdate::AgentMessageChunk(chunk) = update.update
                        && let ContentBlock::Text(text) = chunk.content
                    {
                        if output.len().saturating_add(text.text.len()) <= 4096 {
                            output.push_str(&text.text);
                        } else {
                            overflow = true;
                        }
                    }
                }
                (output, overflow)
            });
            let session = connection
                .client()
                .request(NewSessionRequest::new(workspace.path()))
                .await;
            if let Err(GuestError::Protocol(error)) = &session {
                eprintln!("Session startup ACP code: {}", error.code);
                eprintln!(
                    "ACP error categories: {:?}",
                    support::diagnostic_categories_for_text(&format!("{error:?}"))
                );
            }
            let prompted =
                if canonical_probe && let Ok(session) = &session {
                    Some(connection.client().request(PromptRequest::new(
                session.session_id.clone(),
                vec![ContentBlock::Text(TextContent::new(
                    "Reply with exactly USER_REQUESTED_OUTPUT and do not use tools.",
                ))],
            )).await)
                } else {
                    None
                };
            let closed = connection.disconnect().await;
            let observed = observer.await;
            session.map_err(|_| "proxied session startup failed; no prompt submitted")?;
            closed.map_err(|_| "ACP disconnect failed")?;
            let (output, overflow) = observed.map_err(|_| "ACP observer failed")?;
            if let Some(prompted) = prompted {
                let response =
                    prompted.map_err(|_| "prompt failed; outcome uncertain, do not replay")?;
                if response.stop_reason != StopReason::EndTurn
                    || overflow
                    || output.trim() != expected
                {
                    return Err("canonical model response did not match".into());
                }
                let values = counters.values.lock().map_err(|_| "counter lock")?;
                if values
                    .get("canonical_replaced")
                    .copied()
                    .unwrap_or_default()
                    == 0
                    || values
                        .get("canonical_mismatch")
                        .copied()
                        .unwrap_or_default()
                        != 0
                {
                    return Err("effective canonical request replacement not established".into());
                }
                eprintln!("Canonical response and effective request replacement verified");
            } else {
                eprintln!("Proxied session creation succeeded; no prompt submitted");
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;
    eprintln!(
        "Runtime diagnostic categories: {:?}",
        agent.diagnostic_categories()
    );
    agent.stop().await?;
    proxy.shutdown();
    eprintln!(
        "ACP proxy status classes: {:?}",
        counters.values.lock().map_err(|_| "counter lock")?
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_require_effective_canonical_instructions() -> Result<(), Box<dyn std::error::Error>>
    {
        let counters = Counters {
            canonical: "owned instructions".into(),
            ..Default::default()
        };
        for request in [
            Some(serde_json::json!({
                "request": {"systemInstruction": {"parts": [{"text": "owned instructions"}]}}
            })),
            None,
        ] {
            counters.record(WiretapExchange {
                method: "POST".into(),
                path: "/v1internal:streamGenerateContent".into(),
                headers: serde_json::Value::Null,
                request,
                status: Some(200),
                response: None,
                response_truncated: false,
            });
        }
        let values = counters.values.lock().map_err(|_| "counter lock")?;
        assert_eq!(values.get("canonical_replaced"), Some(&1));
        assert_eq!(values.get("canonical_mismatch"), Some(&1));
        assert_eq!(values.get("http_2xx"), Some(&2));
        Ok(())
    }
}
