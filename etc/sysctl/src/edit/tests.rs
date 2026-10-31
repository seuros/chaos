use super::*;
use chaos_ipc::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use toml::Value as TomlValue;

/// Backing file for the test persistence adapter. Settings are a database
/// document in production; this only simulates revision-checked storage.
const SETTINGS_FILE: &str = "settings.toml";

fn settings_file(scope: crate::persistence::SettingsScope) -> &'static str {
    match scope {
        crate::persistence::SettingsScope::Installation => SETTINGS_FILE,
        crate::persistence::SettingsScope::Global => "global-settings.toml",
    }
}

/// File-backed test adapter for revision-checked edits.
struct TestPersistence;

static REVISIONS: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, i64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

impl crate::persistence::SettingsPersistence for TestPersistence {
    fn snapshot<'a>(
        &'a self,
        home: &'a Path,
        scope: crate::persistence::SettingsScope,
    ) -> crate::persistence::PersistenceFuture<'a, crate::persistence::SettingsSnapshot> {
        Box::pin(async move {
            let file = home.join(settings_file(scope));
            let settings = toml::from_str(&std::fs::read_to_string(&file).unwrap_or_default())?;
            let revision = *REVISIONS
                .lock()
                .expect("revisions")
                .get(&file)
                .unwrap_or(&0);
            Ok(crate::persistence::SettingsSnapshot { settings, revision })
        })
    }

    fn commit<'a>(
        &'a self,
        home: &'a Path,
        scope: crate::persistence::SettingsScope,
        revision: i64,
        settings: toml::Value,
    ) -> crate::persistence::PersistenceFuture<'a, ()> {
        Box::pin(async move {
            let file = home.join(settings_file(scope));
            let mut revisions = REVISIONS.lock().expect("revisions");
            let current = revisions.entry(file.clone()).or_default();
            anyhow::ensure!(*current == revision, "revision conflict");
            std::fs::write(file, toml::to_string(&settings)?)?;
            *current += 1;
            Ok(())
        })
    }
}

fn tempdir() -> std::io::Result<tempfile::TempDir> {
    crate::persistence::install(std::sync::Arc::new(TestPersistence));
    tempfile::tempdir()
}

fn seed(home: &Path, contents: &str) {
    std::fs::write(home.join(SETTINGS_FILE), contents).expect("seed settings");
}

fn settings(home: &Path) -> TomlValue {
    let raw = std::fs::read_to_string(home.join(SETTINGS_FILE)).expect("read settings");
    toml::from_str(&raw).expect("parse settings")
}

fn string_at<'a>(value: &'a TomlValue, path: &[&str]) -> Option<&'a str> {
    at(value, path).and_then(TomlValue::as_str)
}

fn at<'a>(value: &'a TomlValue, path: &[&str]) -> Option<&'a TomlValue> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

#[test]
fn blocking_set_model_top_level() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetModel {
            model: Some("serpent".to_string()),
            effort: Some(ReasoningEffort::High),
        }],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["model"]), Some("serpent"));
    assert_eq!(string_at(&value, &["model_reasoning_effort"]), Some("high"));
}

#[test]
fn builder_with_edits_applies_custom_paths() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    ConfigEditsBuilder::new(chaos_home)
        .with_edits(vec![ConfigEdit::SetPath {
            segments: vec!["enabled".to_string()],
            value: value(true),
        }])
        .apply_blocking()
        .expect("persist");

    assert_eq!(settings(chaos_home)["enabled"], TomlValue::Boolean(true));
}

#[test]
fn set_model_availability_nux_count_writes_shown_count() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    let shown_count = HashMap::from([("serpent".to_string(), 4)]);

    ConfigEditsBuilder::new(chaos_home)
        .set_model_availability_nux_count(&shown_count)
        .apply_blocking()
        .expect("persist");

    assert_eq!(
        at(
            &settings(chaos_home),
            &["tui", "model_availability_nux", "serpent"]
        ),
        Some(&TomlValue::Integer(4))
    );
}

#[test]
fn blocking_set_model_preserves_inline_table_contents() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"profile = "fast"

profiles = { fast = { model = "gordon", sandbox_mode = "strict" } }
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetModel {
            model: Some("o4-mini".to_string()),
            effort: None,
        }],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(
        string_at(&value, &["profiles", "fast", "sandbox_mode"]),
        Some("strict")
    );
    assert_eq!(
        string_at(&value, &["profiles", "fast", "model"]),
        Some("o4-mini")
    );
}

#[test]
fn batch_write_table_upsert_keeps_sibling_entries() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"approval_policy = "headless"

[mcp_servers.linear]
name = "linear"
url = "https://linear.example"

[mcp_servers.linear.http_headers]
foo = "bar"

[sandbox_workspace_write]
network_access = false
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[
            ConfigEdit::SetPath {
                segments: vec![
                    "mcp_servers".to_string(),
                    "linear".to_string(),
                    "url".to_string(),
                ],
                value: value("https://linear.example/v2"),
            },
            ConfigEdit::SetPath {
                segments: vec![
                    "sandbox_workspace_write".to_string(),
                    "network_access".to_string(),
                ],
                value: value(true),
            },
        ],
    )
    .expect("apply");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["approval_policy"]), Some("headless"));
    assert_eq!(
        string_at(&value, &["mcp_servers", "linear", "url"]),
        Some("https://linear.example/v2")
    );
    assert_eq!(
        string_at(&value, &["mcp_servers", "linear", "http_headers", "foo"]),
        Some("bar")
    );
    assert_eq!(
        at(&value, &["sandbox_workspace_write", "network_access"]),
        Some(&TomlValue::Boolean(true))
    );
}

#[test]
fn blocking_clear_model_removes_inline_table_entry() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"profile = "fast"

profiles = { fast = { model = "gordon", sandbox_mode = "strict" } }
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetModel {
            model: None,
            effort: Some(ReasoningEffort::High),
        }],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["profile"]), Some("fast"));
    assert_eq!(at(&value, &["profiles", "fast", "model"]), None);
    assert_eq!(
        string_at(&value, &["profiles", "fast", "sandbox_mode"]),
        Some("strict")
    );
    assert_eq!(
        string_at(&value, &["profiles", "fast", "model_reasoning_effort"]),
        Some("high")
    );
}

#[test]
fn blocking_set_model_scopes_to_active_profile() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"profile = "team"

[profiles.team]
model_reasoning_effort = "low"
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetModel {
            model: Some("o5-preview".to_string()),
            effort: Some(ReasoningEffort::Minimal),
        }],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(at(&value, &["model"]), None);
    assert_eq!(
        string_at(&value, &["profiles", "team", "model"]),
        Some("o5-preview")
    );
    assert_eq!(
        string_at(&value, &["profiles", "team", "model_reasoning_effort"]),
        Some("minimal")
    );
}

#[test]
fn blocking_set_model_with_explicit_profile() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"[profiles."team a"]
model = "serpent"
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        Some("team a"),
        &[ConfigEdit::SetModel {
            model: Some("o4-mini".to_string()),
            effort: None,
        }],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(at(&value, &["model"]), None);
    assert_eq!(
        string_at(&value, &["profiles", "team a", "model"]),
        Some("o4-mini")
    );
}

#[test]
fn blocking_set_hide_full_access_warning_preserves_table() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"[notice]
existing = "value"
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetNoticeHideFullAccessWarning(true)],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["notice", "existing"]), Some("value"));
    assert_eq!(
        at(&value, &["notice", "hide_full_access_warning"]),
        Some(&TomlValue::Boolean(true))
    );
}

#[test]
fn blocking_set_hide_rate_limit_model_nudge_preserves_table() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();
    seed(
        chaos_home,
        r#"[notice]
existing = "value"
"#,
    );

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetNoticeHideRateLimitModelNudge(true)],
    )
    .expect("persist");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["notice", "existing"]), Some("value"));
    assert_eq!(
        at(&value, &["notice", "hide_rate_limit_model_nudge"]),
        Some(&TomlValue::Boolean(true))
    );
}

#[test]
fn blocking_clear_path_noop_when_missing() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::ClearPath {
            segments: vec!["missing".to_string()],
        }],
    )
    .expect("apply");

    assert!(
        !chaos_home.join(SETTINGS_FILE).exists(),
        "a no-op edit must not commit a new settings revision"
    );
}

#[test]
fn blocking_set_path_updates_notifications() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    apply_blocking(
        chaos_home,
        crate::persistence::SettingsScope::Installation,
        None,
        &[ConfigEdit::SetPath {
            segments: vec!["tui".to_string(), "notifications".to_string()],
            value: value(false),
        }],
    )
    .expect("apply");

    assert_eq!(
        at(&settings(chaos_home), &["tui", "notifications"]),
        Some(&TomlValue::Boolean(false))
    );
}

#[tokio::test]
async fn async_builder_set_model_persists() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path().to_path_buf();

    ConfigEditsBuilder::new(&chaos_home)
        .set_model(Some("serpent"), Some(ReasoningEffort::High))
        .apply()
        .await
        .expect("persist");

    let value = settings(&chaos_home);
    assert_eq!(string_at(&value, &["model"]), Some("serpent"));
    assert_eq!(string_at(&value, &["model_reasoning_effort"]), Some("high"));
}

#[test]
fn blocking_builder_set_model_round_trips_back_and_forth() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    ConfigEditsBuilder::new(chaos_home)
        .set_model(Some("o4-mini"), Some(ReasoningEffort::Low))
        .apply_blocking()
        .expect("persist initial");
    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["model"]), Some("o4-mini"));
    assert_eq!(string_at(&value, &["model_reasoning_effort"]), Some("low"));

    ConfigEditsBuilder::new(chaos_home)
        .set_model(Some("serpent"), Some(ReasoningEffort::High))
        .apply_blocking()
        .expect("persist update");
    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["model"]), Some("serpent"));
    assert_eq!(string_at(&value, &["model_reasoning_effort"]), Some("high"));

    ConfigEditsBuilder::new(chaos_home)
        .set_model(Some("o4-mini"), Some(ReasoningEffort::Low))
        .apply_blocking()
        .expect("persist revert");
    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["model"]), Some("o4-mini"));
    assert_eq!(string_at(&value, &["model_reasoning_effort"]), Some("low"));
}

#[tokio::test]
async fn blocking_set_asynchronous_helpers_available() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path().to_path_buf();

    ConfigEditsBuilder::new(&chaos_home)
        .set_hide_full_access_warning(true)
        .apply()
        .await
        .expect("persist");

    assert_eq!(
        at(
            &settings(&chaos_home),
            &["notice", "hide_full_access_warning"]
        ),
        Some(&TomlValue::Boolean(true))
    );
}

#[test]
fn blocking_builder_set_realtime_audio_persists_and_clears() {
    let tmp = tempdir().expect("tmpdir");
    let chaos_home = tmp.path();

    ConfigEditsBuilder::new(chaos_home)
        .set_realtime_microphone(Some("USB Mic"))
        .set_realtime_speaker(Some("Desk Speakers"))
        .apply_blocking()
        .expect("persist realtime audio");

    let value = settings(chaos_home);
    assert_eq!(string_at(&value, &["audio", "microphone"]), Some("USB Mic"));
    assert_eq!(
        string_at(&value, &["audio", "speaker"]),
        Some("Desk Speakers")
    );

    ConfigEditsBuilder::new(chaos_home)
        .set_realtime_microphone(None)
        .apply_blocking()
        .expect("clear realtime microphone");

    let value = settings(chaos_home);
    assert_eq!(at(&value, &["audio", "microphone"]), None);
    assert_eq!(
        string_at(&value, &["audio", "speaker"]),
        Some("Desk Speakers")
    );
}
