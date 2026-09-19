//! Synthetic live checks for the console and CLI.

use std::fmt;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context as _;
use chaos_reflex::Judgment;
use chaos_reflex::Reflex;
use chaos_reflex::Verdict;
use serde_json::json;

use super::build_backend;
use super::configuration;
use super::decide;
use crate::AuthManager;
use crate::arc_monitor::ArcMonitorOutcome;
use crate::config::Config;
use crate::default_client::build_http_client;

#[derive(Debug, Clone)]
pub struct TestReport {
    pub verdict: Verdict,
    /// Configured model route or alias.
    pub model: String,
    /// Request time including retries, excluding credential resolution.
    pub elapsed: Duration,
}

impl fmt::Display for TestReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decision = match decide(&self.verdict) {
            ArcMonitorOutcome::Ok => "allow",
            ArcMonitorOutcome::AskUser(_) => "ask user",
            ArcMonitorOutcome::SteerModel(_) => "block",
            ArcMonitorOutcome::Disabled | ArcMonitorOutcome::Unavailable(_) => "unavailable",
        };
        writeln!(
            f,
            "Reflex test: {:?} · model {:?}",
            self.verdict.backend, self.model
        )?;
        writeln!(
            f,
            "Verdict: {decision} · normalized risk {:.4} · confidence {:.4}",
            self.verdict.probability, self.verdict.confidence
        )?;
        writeln!(
            f,
            "Latency: {} ms (including retries)",
            self.elapsed.as_millis()
        )?;
        for (name, value) in &self.verdict.signals {
            writeln!(f, "{name}: {value:.4}")?;
        }
        write!(f, "Synthetic read-only action; no tool was executed.")
    }
}

/// Test the first Jev backend with synthetic input and no fallback.
pub async fn test(config: &Config, auth: Option<&AuthManager>) -> anyhow::Result<TestReport> {
    let (name, settings) = super::action_risk_settings(config)
        .context("No action-risk backend configured. Open /reflex to add Jev.")?;
    let api_key = configuration::resolve_api_key(config, settings, auth)
        .with_context(|| format!("Reflex test could not initialize backend {name:?}"))?;
    let backend = build_backend(name, settings, build_http_client(), api_key)
        .context("Reflex test could not build the configured backend")?;
    let reflex = Reflex::new(vec![backend]);
    let judgment = Judgment::ActionRisk {
        conversation: json!([{
            "role": "user",
            "content": "Read the local README.md file. Do not change anything."
        }]),
        action: json!({
            "type": "mcp_tool_call",
            "tool_name": "read_file",
            "arguments": { "file_path": "README.md" }
        }),
        instructions: None,
    };
    let start = Instant::now();
    let result = reflex.judge(judgment).await;
    let elapsed = start.elapsed();
    let verdict = result.map_err(|err| {
        anyhow::anyhow!(
            "Reflex test for {name:?} failed after {} ms ({}); no fallback was attempted",
            elapsed.as_millis(),
            err.category()
        )
    })?;
    Ok(TestReport {
        verdict,
        model: settings.model().to_string(),
        elapsed,
    })
}
