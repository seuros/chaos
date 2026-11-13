//! Operator management of project trust decisions.
//!
//! Project trust gates project-scoped configuration and hooks. Console onboarding asks
//! one question per directory; hosted operators need the same decision without a
//! terminal. Everything here is compare-and-set against the stored decision, bound to
//! this installation's identity, and refused from agent shells: an agent cannot grant
//! itself project trust through this interface.
use anyhow::{Context, ensure};
use chaos_ipc::config_types::TrustLevel;
use chaos_proc::{ProjectTrustAudit, ProjectTrustChange, RuntimeDbHandle};
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
    /// `None` when this home has no installation identity yet; reading never creates one.
    pub installation_id: Option<String>,
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
    /// Grants that fail the grant guards. They are not written; a caller that
    /// needs every entry carried must treat a non-empty list as failure. Always
    /// serialized, so a preflight reads an explicit empty list.
    pub held: Vec<HeldEntry>,
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
    pub actor: String,
    pub applied_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    pub changes: Vec<PlannedChange>,
    pub unchanged: Vec<String>,
    pub held: Vec<HeldEntry>,
}

/// A manifest grant that was not written, and why.
#[derive(Debug, Clone, Serialize)]
pub struct HeldEntry {
    pub project_path: String,
    pub reason: String,
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

/// Checks every entry must pass: an absolute, lexically normal path. Denials
/// need nothing more, so a grant on `~` or on a directory that no longer exists
/// can always be revoked.
fn lexical_project_path(raw: &str) -> anyhow::Result<PathBuf> {
    use std::path::Component;
    let path = PathBuf::from(raw);
    ensure!(path.is_absolute(), "project path must be absolute: {raw}");
    // `components()` silently drops `.` and trailing slashes, so compare the
    // rebuilt path too: a key must be spelled exactly as it is stored.
    ensure!(
        path.components().all(|c| matches!(
            c,
            Component::RootDir | Component::Prefix(_) | Component::Normal(_)
        )) && path.components().collect::<PathBuf>().to_string_lossy() == raw,
        "project path {raw:?} is not normalized"
    );
    Ok(path)
}

/// Extra checks for a grant: the directory exists, the stored key is its exact
/// canonical form, it is the gate root the hooks read (its Git root, or itself
/// outside a repository), and it neither contains nor sits inside CHAOS_HOME.
/// `set` and manifest apply share this check, so a grant `set` refuses is held
/// on apply, naming the real root; it is never silently retargeted.
fn grant_guard(raw: &str, home: &Path) -> anyhow::Result<()> {
    let normalized = normalize_project_path(Path::new(raw))?;
    ensure!(
        normalized.to_string_lossy() == raw,
        "path {raw:?} is not normalized; its exact key is {}",
        normalized.display()
    );
    let root = trust_key(&normalized)?;
    ensure!(
        root == normalized,
        "trust for {raw} is decided at its project root {}; grant that path instead",
        root.display()
    );
    let home = std::fs::canonicalize(home)
        .with_context(|| format!("cannot resolve CHAOS_HOME {}", home.display()))?;
    ensure!(
        !home.starts_with(&normalized) && !normalized.starts_with(&home),
        "refusing to trust a path that contains or is inside CHAOS_HOME: {raw}"
    );
    Ok(())
}

/// The key the trust gates read for an existing directory: its Git root, or the
/// directory itself outside a repository. Hooks and onboarding use the same key.
pub fn trust_key(path: &Path) -> anyhow::Result<PathBuf> {
    let root = crate::hooks::project_root(path)?;
    Ok(crate::runtime_db::normalize_cwd_for_runtime_db(&root))
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
    let installation_id = crate::user_settings::existing_installation_id(home)?;
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
    let installation_id = listing.installation_id.context(
        "this home has no installation identity, so it has no decisions of its own to export",
    )?;
    ensure!(
        listing.entries.len() <= MAX_MANIFEST_ENTRIES,
        "{} stored decisions exceed the {MAX_MANIFEST_ENTRIES} a manifest can carry",
        listing.entries.len()
    );
    Ok(Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id,
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
    let raw = match normalize_project_path(project) {
        Ok(path) => {
            let raw = path.to_string_lossy().to_string();
            // Grants are checked against the gate root in `grant_guard`. A denial
            // below the root is refused unless a decision is stored under exactly
            // this key: the config loader reads exact-directory rows, so an old
            // grant left there (say, from before the directory joined a
            // repository) must stay revocable in place, not be redirected.
            if decision == Decision::Untrusted {
                let key = trust_key(&path)?;
                if key != path {
                    let db = crate::user_settings::open(home).await?;
                    ensure!(
                        current_decisions(&db).await?.contains_key(&raw),
                        "trust for {} is decided at its project root {}; set that path instead",
                        path.display(),
                        key.display()
                    );
                }
            }
            raw
        }
        Err(err) => {
            // A path that no longer resolves can only be denied, and only where a
            // decision is stored under exactly that key.
            ensure!(
                decision == Decision::Untrusted,
                "cannot grant trust to {}: {err:#}",
                project.display()
            );
            let raw = lexical_project_path(&project.to_string_lossy())?
                .to_string_lossy()
                .to_string();
            let db = crate::user_settings::open(home).await?;
            ensure!(
                current_decisions(&db).await?.contains_key(&raw),
                "{raw} does not resolve and has no stored decision to revoke"
            );
            raw
        }
    };
    // An explicit operator decision may create the home's identity; the audit
    // row needs one.
    let installation_id = crate::user_settings::installation_id(home)?;
    let prepared = plan(
        home,
        installation_id,
        vec![ManifestEntry {
            project_path: raw,
            decision,
            expect: Some(expect),
        }],
        None,
    )
    .await?;
    if let Some(held) = prepared.held.first() {
        anyhow::bail!("{}: {}", held.project_path, held.reason);
    }
    Ok(prepared)
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
    // Read-only: a plan is a preflight and must not create an identity. A home
    // without one cannot be the installation a manifest names.
    let installation_id = crate::user_settings::existing_installation_id(home)?
        .context("this home has no installation identity, so no manifest can belong to it")?;
    ensure!(
        manifest.installation_id == installation_id,
        "manifest belongs to installation {}, this is {installation_id}; trust decisions do not cross installations",
        manifest.installation_id
    );
    ensure!(
        manifest.entries.len() <= MAX_MANIFEST_ENTRIES,
        "manifest has more than {MAX_MANIFEST_ENTRIES} entries"
    );
    plan(home, installation_id, manifest.entries, Some(actual)).await
}

async fn plan(
    home: &Path,
    installation_id: String,
    entries: Vec<ManifestEntry>,
    manifest_digest: Option<String>,
) -> anyhow::Result<PreparedTrustChange> {
    let db = crate::user_settings::open(home).await?;
    let current = current_decisions(&db).await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut changes = Vec::new();
    let mut unchanged = Vec::new();
    let mut checks = Vec::new();
    let mut conflicts = Vec::new();
    let mut held = Vec::new();
    for entry in entries {
        lexical_project_path(&entry.project_path)?;
        ensure!(
            seen.insert(entry.project_path.clone()),
            "duplicate manifest entry for {}",
            entry.project_path
        );
        let desired = entry
            .decision
            .target()
            .with_context(|| format!("manifest entry {}", entry.project_path))?;
        if Path::new(&entry.project_path).parent().is_none() {
            held.push(HeldEntry {
                project_path: entry.project_path,
                reason: "the filesystem root is never written".to_string(),
            });
            continue;
        }
        if entry.decision == Decision::Trusted
            && let Err(err) = grant_guard(&entry.project_path, home)
        {
            held.push(HeldEntry {
                project_path: entry.project_path,
                reason: format!("{err:#}"),
            });
            continue;
        }
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
    // The loader reads an exact-directory row before the root's, so a grant
    // stored below a revoked root would keep that subdirectory trusted.
    let committing: std::collections::BTreeSet<PathBuf> =
        checks.iter().map(|c| c.project_path.clone()).collect();
    let revoked: Vec<PathBuf> = checks
        .iter()
        .filter(|c| c.desired == TrustLevel::Untrusted)
        .map(|c| c.project_path.clone())
        .collect();
    for (path, level) in &current {
        let stored = PathBuf::from(path);
        if *level != TrustLevel::Trusted
            || committing.contains(&stored)
            || !revoked
                .iter()
                .any(|root| stored != *root && stored.starts_with(root))
        {
            continue;
        }
        checks.push(ProjectTrustChange {
            project_path: stored,
            expected: Decision::Trusted.level(),
            desired: TrustLevel::Untrusted,
        });
        changes.push(PlannedChange {
            project_path: path.clone(),
            before: Decision::Trusted,
            after: Decision::Untrusted,
        });
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
        held,
        home: home.to_path_buf(),
        checks,
    })
}

impl PreparedTrustChange {
    /// Commit for an already-authorised operator. `actor` names the authenticated
    /// caller and is recorded with every change in the same transaction.
    pub async fn commit(self, actor: &str) -> anyhow::Result<TrustReceipt> {
        // Unchanged entries are checked too: the receipt claims every reviewed
        // entry was as seen at the moment of commit, not only the ones written.
        if !self.checks.is_empty() {
            let db = crate::user_settings::open(&self.home).await?;
            let audit = ProjectTrustAudit {
                installation_id: self.installation_id.clone(),
                actor: actor.to_string(),
                manifest_digest: self.manifest_digest.clone(),
            };
            db.apply_project_trust_changes(&self.checks, &audit).await?;
        }
        Ok(TrustReceipt {
            format: RECEIPT_FORMAT,
            installation_id: self.installation_id,
            storage: self.storage,
            actor: actor.to_string(),
            applied_at: jiff::Timestamp::now().as_second(),
            manifest_digest: self.manifest_digest,
            changes: self.changes,
            unchanged: self.unchanged,
            held: self.held,
        })
    }
}

#[cfg(test)]
#[path = "trust_admin_tests.rs"]
mod tests;
