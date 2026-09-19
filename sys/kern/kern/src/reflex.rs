//! Action-risk checks before MCP execution.

use chaos_reflex::ActionRiskSignals;
use chaos_reflex::JevBackend;
use chaos_reflex::Judgment;
use chaos_reflex::JudgmentKind;
use chaos_reflex::LocalChat;
use chaos_reflex::MiniCheckBackend;
use chaos_reflex::Reflex;
use chaos_reflex::ReflexBackend;
use chaos_reflex::ShieldGemmaBackend;
use chaos_reflex::Verdict;
use serde_json::Value;
use tracing::debug;
use tracing::warn;

use crate::arc_monitor::ArcMonitorOutcome;
use crate::config::Config;
use crate::config::ReflexBackendSettings;
use crate::config::ReflexKind;
use crate::default_client::build_http_client;

pub mod configuration;
pub mod diagnostics;

/// Serialized conversation limit in UTF-8 bytes.
const CONVERSATION_BYTE_BUDGET: usize = 96_000;

const BLOCK_EXFILTRATION_PROBABILITY: f64 = 0.85;
const BLOCK_RISK_PROBABILITY: f64 = 0.85;
const BLOCK_RISK_CONFIDENCE: f64 = 0.6;
const ASK_SIGNAL_PROBABILITY: f64 = 0.6;
const ASK_RISK_PROBABILITY: f64 = 0.5;
const LOW_CONFIDENCE: f64 = 0.25;

/// First configured Jev backend, in name order.
pub(crate) fn action_risk_settings(config: &Config) -> Option<(&str, &ReflexBackendSettings)> {
    config
        .reflex
        .iter()
        .find(|(_, settings)| settings.kind == ReflexKind::Jev)
        .map(|(name, settings)| (name.as_str(), settings))
}

pub(crate) fn from_config(
    config: &Config,
    auth: Option<&crate::AuthManager>,
) -> Result<Option<Reflex>, &'static str> {
    let Some((name, settings)) = action_risk_settings(config) else {
        return Ok(None);
    };
    let api_key = configuration::resolve_api_key(config, settings, auth)
        .map_err(|_| "credentials_or_configuration")?;
    let backend = build_backend(name, settings, build_http_client(), api_key)
        .ok_or("backend_initialization")?;
    Ok(Some(Reflex::new(vec![backend])))
}

fn build_backend(
    name: &str,
    settings: &ReflexBackendSettings,
    http: chaos_client::ChaosHttpClient,
    api_key: Option<String>,
) -> Option<Box<dyn ReflexBackend>> {
    let timeout = settings.timeout();
    match settings.kind {
        ReflexKind::Jev => {
            let api_key = api_key?;
            let base_url = settings
                .base_url
                .as_deref()
                .unwrap_or(chaos_reflex::jev::DEFAULT_BASE_URL);
            let mut client = chaos_reflex::jev::JevClient::new(http, base_url, api_key)
                .with_model(settings.model())
                .with_timeout(timeout);
            if let Some(path) = settings.path.as_deref() {
                client = client.with_path(path);
            }
            Some(Box::new(JevBackend::new(name, client)))
        }
        ReflexKind::Minicheck => Some(Box::new(MiniCheckBackend::new(
            name,
            build_local_chat(name, settings, http, api_key)?,
        ))),
        ReflexKind::Shieldgemma => Some(Box::new(ShieldGemmaBackend::new(
            name,
            build_local_chat(name, settings, http, api_key)?,
        ))),
    }
}

fn build_local_chat(
    name: &str,
    settings: &ReflexBackendSettings,
    http: chaos_client::ChaosHttpClient,
    api_key: Option<String>,
) -> Option<LocalChat> {
    let Some(base_url) = settings
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    else {
        warn!(
            backend = name,
            "reflex backend skipped: base_url is required"
        );
        return None;
    };
    Some(
        LocalChat::new(http, base_url, settings.model())
            .with_api_key(api_key)
            .with_timeout(settings.timeout()),
    )
}

pub(crate) fn joined_instructions(developer: Option<&str>, user: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = [developer, user]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub(crate) async fn assess_action(
    reflex: &Reflex,
    conversation: Vec<Value>,
    action: Value,
    instructions: Option<String>,
) -> ArcMonitorOutcome {
    let Some(backend) = reflex.backend_for(JudgmentKind::ActionRisk) else {
        return ArcMonitorOutcome::Unavailable("unsupported");
    };
    let conversation = bounded_conversation(conversation, CONVERSATION_BYTE_BUDGET);
    let judgment = Judgment::ActionRisk {
        conversation: Value::Array(conversation),
        action,
        instructions,
    };
    match reflex.judge(judgment).await {
        Ok(verdict) => {
            debug!(
                backend = %verdict.backend,
                risk = verdict.probability,
                confidence = verdict.confidence,
                signals = ?verdict.signals,
                "reflex action risk completed"
            );
            decide(&verdict)
        }
        Err(err) => {
            warn!(
                backend = backend.name(),
                error_kind = err.category(),
                "reflex action risk unavailable"
            );
            ArcMonitorOutcome::Unavailable(err.category())
        }
    }
}

/// Newest contiguous messages fitting the serialized-array budget.
fn bounded_conversation(mut conversation: Vec<Value>, budget: usize) -> Vec<Value> {
    let mut remaining = budget.saturating_sub(2); // Array brackets.
    let keep = conversation
        .iter()
        .rev()
        .enumerate()
        .take_while(|(index, message)| {
            let size = message.to_string().len() + usize::from(*index > 0);
            if size > remaining {
                return false;
            }
            remaining -= size;
            true
        })
        .count();
    conversation.drain(..conversation.len() - keep);
    conversation
}

pub(crate) fn decide(verdict: &Verdict) -> ArcMonitorOutcome {
    let signal = |name: &str| verdict.signal(name).unwrap_or(0.0);
    let exfiltrates = signal(ActionRiskSignals::EXFILTRATES);
    let irreversible = signal(ActionRiskSignals::IRREVERSIBLE);
    let beyond_request = signal(ActionRiskSignals::BEYOND_REQUEST);

    if exfiltrates >= BLOCK_EXFILTRATION_PROBABILITY {
        return ArcMonitorOutcome::SteerModel(format!(
            "Tool call was cancelled: the reflex rated it likely to send private data externally (p={exfiltrates:.2})."
        ));
    }
    if verdict.probability >= BLOCK_RISK_PROBABILITY && verdict.confidence >= BLOCK_RISK_CONFIDENCE
    {
        return ArcMonitorOutcome::SteerModel(format!(
            "Tool call was cancelled: the reflex rated it destructive (risk={:.2}, confidence={:.2}).",
            verdict.probability, verdict.confidence
        ));
    }
    for (probability, description) in [
        (exfiltrates, "likely to send private data externally"),
        (irreversible, "hard to reverse"),
        (beyond_request, "as going beyond the request"),
    ] {
        if probability >= ASK_SIGNAL_PROBABILITY {
            return ArcMonitorOutcome::AskUser(format!(
                "The reflex rated this tool call {description} (p={probability:.2})."
            ));
        }
    }
    if verdict.probability >= ASK_RISK_PROBABILITY {
        return ArcMonitorOutcome::AskUser(format!(
            "The reflex rated this tool call risky (risk={:.2}).",
            verdict.probability
        ));
    }
    if verdict.confidence < LOW_CONFIDENCE {
        return ArcMonitorOutcome::AskUser(format!(
            "The reflex could not place the risk of this tool call (confidence={:.2}).",
            verdict.confidence
        ));
    }
    ArcMonitorOutcome::Ok
}

#[cfg(test)]
mod tests;
