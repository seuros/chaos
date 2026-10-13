//! User exec-policy rules stored in the runtime database.
//!
//! The document is a JSON array of `{ "name", "source" }` entries (older
//! installations may hold bare strings). Stored rules can only restrict:
//! grants come from administrator policy or remembered approvals.
use anyhow::{Context, bail, ensure};
use chaos_selinux::{Decision, Policy, PolicyParser};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::open;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRule {
    pub name: String,
    pub source: String,
}

/// Decode the stored document, accepting the legacy bare-string shape.
pub fn parse_policy_sources(value: &serde_json::Value) -> anyhow::Result<Vec<UserRule>> {
    let entries = value
        .as_array()
        .context("user policy sources must be an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| match entry {
            serde_json::Value::String(source) => Ok(UserRule {
                name: format!("legacy-{}", index + 1),
                source: source.clone(),
            }),
            other => serde_json::from_value(other.clone()).context("invalid user rule entry"),
        })
        .collect()
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    ensure!(
        (1..=64).contains(&name.len())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "rule names use 1-64 characters from [A-Za-z0-9-_.]"
    );
    Ok(())
}

/// Parse a rule document and reject anything that would widen authority.
pub fn validate_user_rule(name: &str, source: &str) -> anyhow::Result<Policy> {
    validate_name(name)?;
    let mut parser = PolicyParser::new();
    parser.parse(name, source)?;
    let policy = parser.build();
    ensure!(
        policy.host_executables().is_empty(),
        "user rules cannot define executable identities; put them in administrator policy"
    );
    let grants = !policy.get_allowed_prefixes().is_empty()
        || policy
            .network_rules()
            .iter()
            .any(|rule| rule.decision == Decision::Allow);
    if grants {
        bail!(
            "user rules can only restrict; `allow` decisions come from administrator policy \
             (/etc/chaos/rules) or `chaos approvals`"
        );
    }
    Ok(policy)
}

pub async fn list_user_rules(home: &Path) -> anyhow::Result<Vec<UserRule>> {
    let runtime = open(home).await?;
    parse_policy_sources(&runtime.user_policy_sources().await?)
}

/// The rules together with the settings revision that guards their
/// replacement.
pub(super) struct RulesSnapshot {
    settings: chaos_proc::SettingsSnapshot,
    pub(super) rules: Vec<UserRule>,
}

/// Read the revision first, then the document. Every write to the policy
/// document bumps the settings revision in the same transaction, so a commit
/// bound to this revision fails if anyone else wrote after this read; reading
/// in the other order would let a concurrent import be silently overwritten.
pub(super) async fn snapshot(home: &Path) -> anyhow::Result<RulesSnapshot> {
    let runtime = open(home).await?;
    let settings = runtime.settings_snapshot().await?;
    let rules = parse_policy_sources(&runtime.user_policy_sources().await?)?;
    Ok(RulesSnapshot { settings, rules })
}

/// Store or replace the rule named `name`.
pub async fn put_user_rule(home: &Path, name: &str, source: &str) -> anyhow::Result<()> {
    validate_user_rule(name, source)?;
    let snapshot = snapshot(home).await?;
    let mut rules = snapshot.rules.clone();
    rules.retain(|rule| rule.name != name);
    rules.push(UserRule {
        name: name.to_owned(),
        source: source.to_owned(),
    });
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    commit(home, &snapshot, &rules).await
}

pub async fn remove_user_rule(home: &Path, name: &str) -> anyhow::Result<()> {
    let snapshot = snapshot(home).await?;
    let mut rules = snapshot.rules.clone();
    let before = rules.len();
    rules.retain(|rule| rule.name != name);
    ensure!(rules.len() < before, "no stored rule named `{name}`");
    commit(home, &snapshot, &rules).await
}

/// Replace the document, compare-and-swapping on the snapshot's revision.
pub(super) async fn commit(
    home: &Path,
    snapshot: &RulesSnapshot,
    rules: &[UserRule],
) -> anyhow::Result<()> {
    let runtime = open(home).await?;
    runtime
        .commit_settings_import(
            snapshot.settings.revision,
            &snapshot.settings.settings,
            None,
            &[],
            Some(&serde_json::to_value(rules)?),
        )
        .await?;
    Ok(())
}

/// Every stored rule merged as restrictions. They can only narrow authority.
pub(crate) async fn database_user_restrictions(home: &Path) -> anyhow::Result<Policy> {
    let mut policy = Policy::empty();
    for rule in list_user_rules(home).await? {
        let mut parser = PolicyParser::new();
        parser.parse(&rule.name, &rule.source)?;
        let parsed = parser.build();
        ensure!(
            parsed.host_executables().is_empty(),
            "user policy cannot redefine executable identities"
        );
        policy = policy.merge_overlay(&parsed.restrictions_only());
    }
    Ok(policy)
}
