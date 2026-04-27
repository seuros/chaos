use crate::CONFIG_TOML_FILE;
use crate::features::FEATURES;
use crate::path_utils::resolve_symlink_write_paths;
use crate::path_utils::write_atomically;
use crate::types::Notice;
use anyhow::Context;
use chaos_ipc::config_types::Personality;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::openai_models::ReasoningEffort;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tokio::task;
use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;
use toml_edit::Table as TomlTable;
use toml_edit::value;

/// Discrete config mutations supported by the persistence engine.
#[derive(Clone, Debug)]
pub enum ConfigEdit {
    /// Update the active (or default) model selection and optional reasoning effort.
    SetModel {
        model: Option<String>,
        effort: Option<ReasoningEffort>,
    },
    /// Update the service tier preference for future turns.
    SetServiceTier { service_tier: Option<ServiceTier> },
    /// Update the active (or default) model personality.
    SetModelPersonality { personality: Option<Personality> },
    /// Toggle the acknowledgement flag under `[notice]`.
    SetNoticeHideFullAccessWarning(bool),
    /// Toggle the Windows world-writable directories warning acknowledgement flag.
    SetNoticeHideWorldWritableWarning(bool),
    /// Toggle the rate limit model nudge acknowledgement flag.
    SetNoticeHideRateLimitModelNudge(bool),
    /// Set the value stored at the exact dotted path.
    SetPath {
        segments: Vec<String>,
        value: TomlItem,
    },
    /// Remove the value stored at the exact dotted path.
    ClearPath { segments: Vec<String> },
}

/// Produces a config edit that sets `[tui] theme = "<name>"`.
pub fn syntax_theme_edit(name: &str) -> ConfigEdit {
    ConfigEdit::SetPath {
        segments: vec!["tui".to_string(), "theme".to_string()],
        value: value(name.to_string()),
    }
}

pub fn status_line_items_edit(items: &[String]) -> ConfigEdit {
    let mut array = toml_edit::Array::new();
    for item in items {
        array.push(item.clone());
    }

    ConfigEdit::SetPath {
        segments: vec!["tui".to_string(), "status_line".to_string()],
        value: TomlItem::Value(array.into()),
    }
}

pub fn model_availability_nux_count_edits(shown_count: &HashMap<String, u32>) -> Vec<ConfigEdit> {
    let mut shown_count_entries: Vec<_> = shown_count.iter().collect();
    shown_count_entries.sort_unstable_by_key(|(left, _)| *left);

    let mut edits = vec![ConfigEdit::ClearPath {
        segments: vec!["tui".to_string(), "model_availability_nux".to_string()],
    }];
    for (model_slug, count) in shown_count_entries {
        edits.push(ConfigEdit::SetPath {
            segments: vec![
                "tui".to_string(),
                "model_availability_nux".to_string(),
                model_slug.clone(),
            ],
            value: value(i64::from(*count)),
        });
    }

    edits
}

// TODO(jif) move to a dedicated file
mod document_helpers {
    use toml_edit::InlineTable;
    use toml_edit::Item as TomlItem;
    use toml_edit::Table as TomlTable;

    pub(super) fn ensure_table_for_write(item: &mut TomlItem) -> Option<&mut TomlTable> {
        match item {
            TomlItem::Table(table) => Some(table),
            TomlItem::Value(value) => {
                if let Some(inline) = value.as_inline_table() {
                    *item = TomlItem::Table(table_from_inline(inline));
                    item.as_table_mut()
                } else {
                    *item = TomlItem::Table(new_implicit_table());
                    item.as_table_mut()
                }
            }
            TomlItem::None => {
                *item = TomlItem::Table(new_implicit_table());
                item.as_table_mut()
            }
            _ => None,
        }
    }

    pub(super) fn ensure_table_for_read(item: &mut TomlItem) -> Option<&mut TomlTable> {
        match item {
            TomlItem::Table(table) => Some(table),
            TomlItem::Value(value) => {
                let inline = value.as_inline_table()?;
                *item = TomlItem::Table(table_from_inline(inline));
                item.as_table_mut()
            }
            _ => None,
        }
    }

    fn table_from_inline(inline: &InlineTable) -> TomlTable {
        let mut table = new_implicit_table();
        for (key, value) in inline.iter() {
            let mut value = value.clone();
            let decor = value.decor_mut();
            decor.set_suffix("");
            table.insert(key, TomlItem::Value(value));
        }
        table
    }

    pub(super) fn new_implicit_table() -> TomlTable {
        let mut table = TomlTable::new();
        table.set_implicit(true);
        table
    }
}

struct ConfigDocument {
    doc: DocumentMut,
    profile: Option<String>,
}

#[derive(Copy, Clone)]
enum Scope {
    Global,
    Profile,
}

#[derive(Copy, Clone)]
enum TraversalMode {
    Create,
    Existing,
}

impl ConfigDocument {
    fn new(doc: DocumentMut, profile: Option<String>) -> Self {
        Self { doc, profile }
    }

    fn apply(&mut self, edit: &ConfigEdit) -> anyhow::Result<bool> {
        match edit {
            ConfigEdit::SetModel { model, effort } => Ok({
                let mut mutated = false;
                mutated |= self.write_profile_value(
                    &["model"],
                    model.as_ref().map(|model_value| value(model_value.clone())),
                );
                mutated |= self.write_profile_value(
                    &["model_reasoning_effort"],
                    effort.map(|effort| value(effort.to_string())),
                );
                mutated
            }),
            ConfigEdit::SetServiceTier { service_tier } => Ok(self.write_profile_value(
                &["service_tier"],
                service_tier.map(|service_tier| value(service_tier.to_string())),
            )),
            ConfigEdit::SetModelPersonality { personality } => Ok(self.write_profile_value(
                &["personality"],
                personality.map(|personality| value(personality.to_string())),
            )),
            ConfigEdit::SetNoticeHideFullAccessWarning(acknowledged) => Ok(self.write_value(
                Scope::Global,
                &[Notice::TABLE_KEY, "hide_full_access_warning"],
                value(*acknowledged),
            )),
            ConfigEdit::SetNoticeHideWorldWritableWarning(acknowledged) => Ok(self.write_value(
                Scope::Global,
                &[Notice::TABLE_KEY, "hide_world_writable_warning"],
                value(*acknowledged),
            )),
            ConfigEdit::SetNoticeHideRateLimitModelNudge(acknowledged) => Ok(self.write_value(
                Scope::Global,
                &[Notice::TABLE_KEY, "hide_rate_limit_model_nudge"],
                value(*acknowledged),
            )),
            ConfigEdit::SetPath { segments, value } => Ok(self.insert(segments, value.clone())),
            ConfigEdit::ClearPath { segments } => Ok(self.clear_owned(segments)),
        }
    }

    fn write_profile_value(&mut self, segments: &[&str], value: Option<TomlItem>) -> bool {
        match value {
            Some(item) => self.write_value(Scope::Profile, segments, item),
            None => self.clear(Scope::Profile, segments),
        }
    }

    fn write_value(&mut self, scope: Scope, segments: &[&str], value: TomlItem) -> bool {
        let resolved = self.scoped_segments(scope, segments);
        self.insert(&resolved, value)
    }

    fn clear(&mut self, scope: Scope, segments: &[&str]) -> bool {
        let resolved = self.scoped_segments(scope, segments);
        self.remove(&resolved)
    }

    fn clear_owned(&mut self, segments: &[String]) -> bool {
        self.remove(segments)
    }

    fn scoped_segments(&self, scope: Scope, segments: &[&str]) -> Vec<String> {
        let resolved: Vec<String> = segments
            .iter()
            .map(|segment| (*segment).to_string())
            .collect();

        if matches!(scope, Scope::Profile)
            && resolved.first().is_none_or(|segment| segment != "profiles")
            && let Some(profile) = self.profile.as_deref()
        {
            let mut scoped = Vec::with_capacity(resolved.len() + 2);
            scoped.push("profiles".to_string());
            scoped.push(profile.to_string());
            scoped.extend(resolved);
            return scoped;
        }

        resolved
    }

    fn insert(&mut self, segments: &[String], value: TomlItem) -> bool {
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };

        let Some(parent) = self.descend(parents, TraversalMode::Create) else {
            return false;
        };

        let mut value = value;
        if let Some(existing) = parent.get(last) {
            Self::preserve_decor(existing, &mut value);
        }
        parent[last] = value;
        true
    }

    fn remove(&mut self, segments: &[String]) -> bool {
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };

        let Some(parent) = self.descend(parents, TraversalMode::Existing) else {
            return false;
        };

        parent.remove(last).is_some()
    }

    fn descend(&mut self, segments: &[String], mode: TraversalMode) -> Option<&mut TomlTable> {
        let mut current = self.doc.as_table_mut();

        for segment in segments {
            match mode {
                TraversalMode::Create => {
                    if !current.contains_key(segment.as_str()) {
                        current.insert(
                            segment.as_str(),
                            TomlItem::Table(document_helpers::new_implicit_table()),
                        );
                    }

                    let item = current.get_mut(segment.as_str())?;
                    current = document_helpers::ensure_table_for_write(item)?;
                }
                TraversalMode::Existing => {
                    let item = current.get_mut(segment.as_str())?;
                    current = document_helpers::ensure_table_for_read(item)?;
                }
            }
        }

        Some(current)
    }

    fn preserve_decor(existing: &TomlItem, replacement: &mut TomlItem) {
        match (existing, replacement) {
            (TomlItem::Table(existing_table), TomlItem::Table(replacement_table)) => {
                replacement_table
                    .decor_mut()
                    .clone_from(existing_table.decor());
                for (key, existing_item) in existing_table.iter() {
                    if let (Some(existing_key), Some(mut replacement_key)) =
                        (existing_table.key(key), replacement_table.key_mut(key))
                    {
                        replacement_key
                            .leaf_decor_mut()
                            .clone_from(existing_key.leaf_decor());
                        replacement_key
                            .dotted_decor_mut()
                            .clone_from(existing_key.dotted_decor());
                    }
                    if let Some(replacement_item) = replacement_table.get_mut(key) {
                        Self::preserve_decor(existing_item, replacement_item);
                    }
                }
            }
            (TomlItem::Value(existing_value), TomlItem::Value(replacement_value)) => {
                replacement_value
                    .decor_mut()
                    .clone_from(existing_value.decor());
            }
            _ => {}
        }
    }
}

/// Persist edits using a blocking strategy.
pub fn apply_blocking(
    chaos_home: &Path,
    profile: Option<&str>,
    edits: &[ConfigEdit],
) -> anyhow::Result<()> {
    if edits.is_empty() {
        return Ok(());
    }

    let config_path = chaos_home.join(CONFIG_TOML_FILE);
    let write_paths = resolve_symlink_write_paths(&config_path)?;
    let serialized = match write_paths.read_path {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        },
        None => String::new(),
    };

    let doc = if serialized.is_empty() {
        DocumentMut::new()
    } else {
        serialized.parse::<DocumentMut>()?
    };

    let profile = profile.map(ToOwned::to_owned).or_else(|| {
        doc.get("profile")
            .and_then(|item| item.as_str())
            .map(ToOwned::to_owned)
    });

    let mut document = ConfigDocument::new(doc, profile);
    let mut mutated = false;

    for edit in edits {
        mutated |= document.apply(edit)?;
    }

    if !mutated {
        return Ok(());
    }

    write_atomically(&write_paths.write_path, &document.doc.to_string()).with_context(|| {
        format!(
            "failed to persist config.toml at {}",
            write_paths.write_path.display()
        )
    })?;

    Ok(())
}

/// Persist edits asynchronously by offloading the blocking writer.
pub async fn apply(
    chaos_home: &Path,
    profile: Option<&str>,
    edits: Vec<ConfigEdit>,
) -> anyhow::Result<()> {
    let chaos_home = chaos_home.to_path_buf();
    let profile = profile.map(ToOwned::to_owned);
    task::spawn_blocking(move || apply_blocking(&chaos_home, profile.as_deref(), &edits))
        .await
        .context("config persistence task panicked")?
}

/// Fluent builder to batch config edits and apply them atomically.
#[derive(Default)]
pub struct ConfigEditsBuilder {
    chaos_home: PathBuf,
    profile: Option<String>,
    edits: Vec<ConfigEdit>,
}

impl ConfigEditsBuilder {
    pub fn new(chaos_home: &Path) -> Self {
        Self {
            chaos_home: chaos_home.to_path_buf(),
            profile: None,
            edits: Vec::new(),
        }
    }

    pub fn with_profile(mut self, profile: Option<&str>) -> Self {
        self.profile = profile.map(ToOwned::to_owned);
        self
    }

    pub fn set_model(mut self, model: Option<&str>, effort: Option<ReasoningEffort>) -> Self {
        self.edits.push(ConfigEdit::SetModel {
            model: model.map(ToOwned::to_owned),
            effort,
        });
        self
    }

    pub fn set_service_tier(mut self, service_tier: Option<ServiceTier>) -> Self {
        self.edits.push(ConfigEdit::SetServiceTier { service_tier });
        self
    }

    pub fn set_personality(mut self, personality: Option<Personality>) -> Self {
        self.edits
            .push(ConfigEdit::SetModelPersonality { personality });
        self
    }

    pub fn set_hide_full_access_warning(mut self, acknowledged: bool) -> Self {
        self.edits
            .push(ConfigEdit::SetNoticeHideFullAccessWarning(acknowledged));
        self
    }

    pub fn set_hide_world_writable_warning(mut self, acknowledged: bool) -> Self {
        self.edits
            .push(ConfigEdit::SetNoticeHideWorldWritableWarning(acknowledged));
        self
    }

    pub fn set_hide_rate_limit_model_nudge(mut self, acknowledged: bool) -> Self {
        self.edits
            .push(ConfigEdit::SetNoticeHideRateLimitModelNudge(acknowledged));
        self
    }

    pub fn set_model_availability_nux_count(mut self, shown_count: &HashMap<String, u32>) -> Self {
        self.edits
            .extend(model_availability_nux_count_edits(shown_count));
        self
    }

    /// Enable or disable a feature flag by key under the `[features]` table.
    ///
    /// Disabling a default-false feature clears the root-scoped key instead of
    /// persisting `false`, so the config does not pin the feature once it
    /// graduates to globally enabled. Profile-scoped disables still persist
    /// `false` so they can override an inherited root enable.
    pub fn set_feature_enabled(mut self, key: &str, enabled: bool) -> Self {
        let profile_scoped = self.profile.is_some();
        let segments = if let Some(profile) = self.profile.as_ref() {
            vec![
                "profiles".to_string(),
                profile.clone(),
                "features".to_string(),
                key.to_string(),
            ]
        } else {
            vec!["features".to_string(), key.to_string()]
        };
        let is_default_false_feature = FEATURES
            .iter()
            .find(|spec| spec.key == key)
            .is_some_and(|spec| !spec.default_enabled);
        if enabled || profile_scoped || !is_default_false_feature {
            self.edits.push(ConfigEdit::SetPath {
                segments,
                value: value(enabled),
            });
        } else {
            self.edits.push(ConfigEdit::ClearPath { segments });
        }
        self
    }

    pub fn set_realtime_microphone(mut self, microphone: Option<&str>) -> Self {
        let segments = vec!["audio".to_string(), "microphone".to_string()];
        match microphone {
            Some(microphone) => self.edits.push(ConfigEdit::SetPath {
                segments,
                value: value(microphone),
            }),
            None => self.edits.push(ConfigEdit::ClearPath { segments }),
        }
        self
    }

    pub fn set_realtime_speaker(mut self, speaker: Option<&str>) -> Self {
        let segments = vec!["audio".to_string(), "speaker".to_string()];
        match speaker {
            Some(speaker) => self.edits.push(ConfigEdit::SetPath {
                segments,
                value: value(speaker),
            }),
            None => self.edits.push(ConfigEdit::ClearPath { segments }),
        }
        self
    }

    pub fn with_edits<I>(mut self, edits: I) -> Self
    where
        I: IntoIterator<Item = ConfigEdit>,
    {
        self.edits.extend(edits);
        self
    }

    /// Apply edits on a blocking thread.
    pub fn apply_blocking(self) -> anyhow::Result<()> {
        apply_blocking(&self.chaos_home, self.profile.as_deref(), &self.edits)
    }

    /// Apply edits asynchronously via a blocking offload.
    pub async fn apply(self) -> anyhow::Result<()> {
        task::spawn_blocking(move || {
            apply_blocking(&self.chaos_home, self.profile.as_deref(), &self.edits)
        })
        .await
        .context("config persistence task panicked")?
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
