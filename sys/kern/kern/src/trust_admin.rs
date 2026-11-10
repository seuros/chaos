//! Operator management of project trust decisions.
//!
//! Project trust gates project-scoped configuration and hooks. Console onboarding asks
//! one question per directory; hosted operators need the same decision without a
//! terminal. Everything here is compare-and-set against the stored decision, bound to
//! this installation's identity, and refused from agent shells: an agent cannot grant
//! itself project trust through this interface.
use anyhow::{Context, ensure};
use chaos_ipc::config_types::TrustLevel;
use chaos_proc::{ProjectTrustChange, RuntimeDbHandle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FORMAT: &str = "chaos-project-trust/v1";
pub const RECEIPT_FORMAT: &str = "chaos-project-trust-receipt/v1";
const MAX_MANIFEST_ENTRIES: usize = 10_000;

/// A stored decision, or the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Trusted,
    Untrusted,
    None,
}

impl Decision {
    /// The level this decision stores. `None` is never written: revocation is
    /// stored as a denial so it cannot be confused with "never decided".
    fn target(self) -> anyhow::Result<TrustLevel> {
        self.level().ok_or_else(|| {
            anyhow::anyhow!(
                "a decision cannot be set to none; revoke stores untrusted so that a stale manifest cannot restore trust"
            )
        })
    }

    fn level(self) -> Option<TrustLevel> {
        match self {
            Self::Trusted => Some(TrustLevel::Trusted),
            Self::Untrusted => Some(TrustLevel::Untrusted),
            Self::None => None,
        }
    }

    fn from_level(level: Option<TrustLevel>) -> Self {
        match level {
            Some(TrustLevel::Trusted) => Self::Trusted,
            Some(TrustLevel::Untrusted) => Self::Untrusted,
            None => Self::None,
        }
    }
}

impl std::str::FromStr for Decision {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "trusted" => Ok(Self::Trusted),
            "untrusted" => Ok(Self::Untrusted),
            "none" => Ok(Self::None),
            other => anyhow::bail!("expected trusted, untrusted or none, got {other:?}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub project_path: String,
    pub decision: Decision,
    /// Required prior decision. When omitted the entry applies only where no
    /// decision is stored, or where the stored decision already equals `decision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Decision>,
}

/// A reviewed batch of decisions for exactly one installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub installation_id: String,
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn digest(&self) -> anyhow::Result<String> {
        crate::user_settings::fingerprint(&serde_json::to_value(self)?)
    }
}

#[derive(Debug, Serialize)]
pub struct TrustListing {
    pub installation_id: String,
    pub storage: &'static str,
    pub entries: Vec<ListedDecision>,
}

#[derive(Debug, Serialize)]
pub struct ListedDecision {
    pub project_path: String,
    pub decision: Decision,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedChange {
    pub project_path: String,
    pub before: Decision,
    pub after: Decision,
}

/// An immutable change set, checked against the stored state when prepared and
/// again, atomically, when committed.
#[derive(Debug, Serialize)]
pub struct PreparedTrustChange {
    pub installation_id: String,
    pub storage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    pub changes: Vec<PlannedChange>,
    pub unchanged: Vec<String>,
    #[serde(skip)]
    home: PathBuf,
    /// Every reviewed entry, unchanged ones included, re-checked atomically at commit.
    #[serde(skip)]
    checks: Vec<ProjectTrustChange>,
}

#[derive(Debug, Serialize)]
pub struct TrustReceipt {
    pub format: &'static str,
    pub installation_id: String,
    pub storage: &'static str,
    pub actor: &'static str,
    pub applied_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    pub changes: Vec<PlannedChange>,
    pub unchanged: Vec<String>,
}

fn storage_kind(db: &RuntimeDbHandle) -> &'static str {
    match db {
        RuntimeDbHandle::Postgres(_) => "postgres",
        RuntimeDbHandle::Sqlite(_) => "sqlite",
    }
}

/// Resolve an operator-supplied directory to the exact key project trust is stored under.
pub fn normalize_project_path(path: &Path) -> anyhow::Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "project path must be absolute: {}",
        path.display()
    );
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("cannot resolve project directory {}", path.display()))?;
    ensure!(
        canonical.is_dir(),
        "project path is not a directory: {}",
        canonical.display()
    );
    let normalized = crate::runtime_db::normalize_cwd_for_runtime_db(&canonical);
    ensure!(
        normalized.parent().is_some(),
        "refusing to change trust for the filesystem root"
    );
    Ok(normalized)
}

fn exact_project_path(raw: &str, home: &Path) -> anyhow::Result<PathBuf> {
    let normalized = normalize_project_path(Path::new(raw))?;
    ensure!(
        normalized.to_string_lossy() == raw,
        "manifest path {raw:?} is not normalized; its exact key is {}",
        normalized.display()
    );
    if let Ok(home) = std::fs::canonicalize(home) {
        ensure!(
            !home.starts_with(&normalized) && !normalized.starts_with(&home),
            "refusing to change trust for a path that contains or is inside CHAOS_HOME: {raw}"
        );
    }
    Ok(normalized)
}

async fn current_decisions(db: &RuntimeDbHandle) -> anyhow::Result<BTreeMap<String, TrustLevel>> {
    Ok(db
        .list_project_trust()
        .await?
        .into_iter()
        .map(|record| (record.project_path, record.trust_level))
        .collect())
}

/// Read every stored decision. Changes nothing.
pub async fn list(home: &Path) -> anyhow::Result<TrustListing> {
    let installation_id = crate::user_settings::installation_id(home)?;
    let db = crate::user_settings::open(home).await?;
    let entries = db
        .list_project_trust()
        .await?
        .into_iter()
        .map(|record| ListedDecision {
            project_path: record.project_path,
            decision: Decision::from_level(Some(record.trust_level)),
            updated_at: record.updated_at,
        })
        .collect();
    Ok(TrustListing {
        installation_id,
        storage: storage_kind(&db),
        entries,
    })
}

/// Every stored decision, granted and denied, as a manifest bound to this installation.
pub async fn export(home: &Path) -> anyhow::Result<Manifest> {
    let listing = list(home).await?;
    Ok(Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: listing.installation_id,
        entries: listing
            .entries
            .into_iter()
            .map(|entry| ManifestEntry {
                project_path: entry.project_path,
                decision: entry.decision,
                expect: None,
            })
            .collect(),
    })
}

/// Prepare one decision for one exact project directory.
pub async fn prepare_set(
    home: &Path,
    project: &Path,
    decision: Decision,
    expect: Decision,
) -> anyhow::Result<PreparedTrustChange> {
    let path = normalize_project_path(project)?;
    let raw = path.to_string_lossy().to_string();
    exact_project_path(&raw, home)?;
    plan(
        home,
        vec![ManifestEntry {
            project_path: raw,
            decision,
            expect: Some(expect),
        }],
        None,
    )
    .await
}

/// Prepare a reviewed manifest. `digest` must equal the manifest's own digest, and the
/// manifest must name this installation.
pub async fn prepare_apply(
    home: &Path,
    manifest_json: &str,
    digest: &str,
) -> anyhow::Result<PreparedTrustChange> {
    let manifest: Manifest =
        serde_json::from_str(manifest_json).context("invalid project trust manifest")?;
    ensure!(
        manifest.format == MANIFEST_FORMAT,
        "unsupported manifest format {:?}; expected {MANIFEST_FORMAT}",
        manifest.format
    );
    let actual = manifest.digest()?;
    ensure!(
        actual == digest,
        "manifest digest mismatch: reviewed {digest}, file is {actual}; review the file again"
    );
    let installation_id = crate::user_settings::installation_id(home)?;
    ensure!(
        manifest.installation_id == installation_id,
        "manifest belongs to installation {}, this is {installation_id}; trust decisions do not cross installations",
        manifest.installation_id
    );
    ensure!(
        manifest.entries.len() <= MAX_MANIFEST_ENTRIES,
        "manifest has more than {MAX_MANIFEST_ENTRIES} entries"
    );
    plan(home, manifest.entries, Some(actual)).await
}

async fn plan(
    home: &Path,
    entries: Vec<ManifestEntry>,
    manifest_digest: Option<String>,
) -> anyhow::Result<PreparedTrustChange> {
    let installation_id = crate::user_settings::installation_id(home)?;
    let db = crate::user_settings::open(home).await?;
    let current = current_decisions(&db).await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut changes = Vec::new();
    let mut unchanged = Vec::new();
    let mut checks = Vec::new();
    let mut conflicts = Vec::new();
    for entry in entries {
        exact_project_path(&entry.project_path, home)?;
        ensure!(
            seen.insert(entry.project_path.clone()),
            "duplicate manifest entry for {}",
            entry.project_path
        );
        let desired = entry
            .decision
            .target()
            .with_context(|| format!("manifest entry {}", entry.project_path))?;
        let found = Decision::from_level(current.get(&entry.project_path).copied());
        let acceptable = match entry.expect {
            Some(expect) => found == expect,
            None => found == Decision::None || found == entry.decision,
        };
        if !acceptable {
            conflicts.push(format!(
                "{}: expected {}, found {}",
                entry.project_path,
                serde_json::to_value(entry.expect.unwrap_or(Decision::None))?
                    .as_str()
                    .unwrap_or_default(),
                serde_json::to_value(found)?.as_str().unwrap_or_default()
            ));
            continue;
        }
        checks.push(ProjectTrustChange {
            project_path: PathBuf::from(&entry.project_path),
            expected: found.level(),
            desired,
        });
        if found == entry.decision {
            unchanged.push(entry.project_path);
        } else {
            changes.push(PlannedChange {
                project_path: entry.project_path,
                before: found,
                after: entry.decision,
            });
        }
    }
    ensure!(
        conflicts.is_empty(),
        "refusing: {} stored decision(s) differ from what was reviewed; nothing was changed:\n{}",
        conflicts.len(),
        conflicts.join("\n")
    );
    Ok(PreparedTrustChange {
        installation_id,
        storage: storage_kind(&db),
        manifest_digest,
        changes,
        unchanged,
        home: home.to_path_buf(),
        checks,
    })
}

impl PreparedTrustChange {
    pub fn approval_message(&self) -> anyhow::Result<String> {
        Ok(format!(
            "Apply these project trust decisions on THIS installation?\n{}\nTrusted projects load their .mcp.json, rules, agents and project hooks.",
            serde_json::to_string_pretty(self)?
        ))
    }

    async fn commit(self) -> anyhow::Result<TrustReceipt> {
        // Unchanged entries are checked too: the receipt claims every reviewed
        // entry was as seen at the moment of commit, not only the ones written.
        if !self.checks.is_empty() {
            let db = crate::user_settings::open(&self.home).await?;
            db.apply_project_trust_changes(&self.checks).await?;
        }
        Ok(TrustReceipt {
            format: RECEIPT_FORMAT,
            installation_id: self.installation_id,
            storage: self.storage,
            actor: "operator-cli",
            applied_at: jiff::Timestamp::now().as_second(),
            manifest_digest: self.manifest_digest,
            changes: self.changes,
            unchanged: self.unchanged,
        })
    }

    /// Commit after the operator approves: interactively, or with explicit `yes`
    /// from a non-agent process.
    pub async fn confirm_terminal(self, yes: bool) -> anyhow::Result<TrustReceipt> {
        confirm_operator(
            &self.approval_message()?,
            yes,
            std::env::var_os(crate::exec_env::CHAOS_THREAD_ID_ENV_VAR).is_some(),
        )?;
        self.commit().await
    }
}

fn confirm_operator(message: &str, yes: bool, agent_shell: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    ensure!(
        !agent_shell,
        "agent shell commands cannot change project trust; this is an operator decision"
    );
    if yes {
        return Ok(());
    }
    ensure!(
        std::io::stdin().is_terminal(),
        "project trust changes require an interactive terminal or explicit --yes"
    );
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{message}")?;
    write!(stdout, "Type yes to approve: ")?;
    stdout.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    ensure!(answer.trim() == "yes", "project trust change cancelled");
    Ok(())
}

#[cfg(test)]
#[path = "trust_admin_tests.rs"]
mod tests;
