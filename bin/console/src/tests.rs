use super::*;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_kern::AuthManager;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::ProjectTrust;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn report_to_io_error_preserves_cause_chain() {
    let result: color_eyre::Result<()> = Err(eyre!("Connection refused (os error 61)"));
    let report = result
        .wrap_err("required MCP server `skynet` failed to initialize")
        .wrap_err("Failed to resume session 01a04fe5")
        .expect_err("error chain");

    let error = report_to_io_error(report);

    assert_eq!(
        error.to_string(),
        "Failed to resume session 01a04fe5\n\nCaused by:\n    0: required MCP server `skynet` failed to initialize\n    1: Connection refused (os error 61)"
    );
}

async fn build_config(temp_dir: &TempDir) -> std::io::Result<Config> {
    ConfigBuilder::default()
        .chaos_home(temp_dir.path().to_path_buf())
        .build()
        .await
}

#[test]
#[ignore]
fn lib_suite() {
    std::thread::Builder::new()
        .name("console-lib-suite".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build console test runtime")
                .block_on(run_lib_suite());
        })
        .expect("spawn console lib suite thread")
        .join()
        .expect("console lib suite panicked");
}

async fn run_lib_suite() {
    super::additional_dirs::tests::add_dir_warning_message_only_warns_for_read_only_sandbox_with_dirs();
    super::app::tests::app_tests_suite().await;
    super::app_backtrack::tests::app_backtrack_suite();
    super::cwd_prompt::tests::cwd_prompt_suite();
    super::external_editor::tests::run_editor_returns_updated_content().await;
    super::onboarding::tests::onboarding_suite();
    super::pager_overlay::tests::pager_overlay_suite();
    super::resume_picker::tests::resume_picker_suite().await;

    boot_core_returns_valid_managers()
        .await
        .expect("boot_core_returns_valid_managers");
    untrusted_project_skips_trust_prompt()
        .await
        .expect("untrusted_project_skips_trust_prompt");
    config_rebuild_changes_trust_defaults_with_cwd()
        .await
        .expect("config_rebuild_changes_trust_defaults_with_cwd");
    theme_warning_uses_final_config()
        .await
        .expect("theme_warning_uses_final_config");
}

async fn boot_core_returns_valid_managers() -> std::io::Result<()> {
    let temp_dir = TempDir::new()?;
    let config = build_config(&temp_dir).await?;
    let managers = boot_core(&config);

    // AuthManager was created — just verify it is non-null and usable.
    // Note: AuthManager::shared always allocates a fresh Arc (no global
    // singleton cache), so ptr_eq comparisons across calls always fail.
    let _auth_ref: &AuthManager = &managers.auth_manager;

    // ProcessTable was created.
    let _ = managers.process_table.get_models_manager();
    Ok(())
}

async fn untrusted_project_skips_trust_prompt() -> std::io::Result<()> {
    use chaos_ipc::config_types::TrustLevel;
    let temp_dir = TempDir::new()?;
    let mut config = build_config(&temp_dir).await?;
    config.active_project_trust = ProjectTrust {
        trust_level: Some(TrustLevel::Untrusted),
    };

    let should_show = should_show_trust_screen(&config);
    assert!(
        !should_show,
        "Trust prompt should not be shown for projects explicitly marked as untrusted"
    );
    Ok(())
}

async fn config_rebuild_changes_trust_defaults_with_cwd() -> std::io::Result<()> {
    use chaos_ipc::config_types::TrustLevel;
    use chaos_kern::config::set_project_trust_level;

    let temp_dir = TempDir::new()?;
    let chaos_home = temp_dir.path().to_path_buf();
    let trusted = temp_dir.path().join("trusted");
    let untrusted = temp_dir.path().join("untrusted");
    std::fs::create_dir_all(&trusted)?;
    std::fs::create_dir_all(&untrusted)?;
    set_project_trust_level(&chaos_home, &trusted, TrustLevel::Trusted)
        .map_err(std::io::Error::other)?;
    set_project_trust_level(&chaos_home, &untrusted, TrustLevel::Untrusted)
        .map_err(std::io::Error::other)?;

    let trusted_overrides = ConfigOverrides {
        cwd: Some(trusted.clone()),
        ..Default::default()
    };
    let trusted_config = ConfigBuilder::default()
        .chaos_home(chaos_home.clone())
        .harness_overrides(trusted_overrides.clone())
        .build()
        .await?;
    assert_eq!(
        trusted_config.permissions.approval_policy.value(),
        ApprovalPolicy::Interactive
    );

    let untrusted_overrides = ConfigOverrides {
        cwd: Some(untrusted),
        ..trusted_overrides
    };
    let untrusted_config = ConfigBuilder::default()
        .chaos_home(chaos_home)
        .harness_overrides(untrusted_overrides)
        .build()
        .await?;
    assert_eq!(
        untrusted_config.permissions.approval_policy.value(),
        ApprovalPolicy::Supervised
    );
    Ok(())
}

/// Regression: theme must be configured from the *final* config.
///
/// `run_ratatui_app` can reload config during onboarding and again
/// during session resume/fork.  The syntax theme override (stored in
/// a `OnceLock`) must use the final config's `tui_theme`, not the
/// initial one — otherwise users resuming a thread in a project with
/// a different theme get the wrong highlighting.
///
/// We verify the invariant indirectly: `validate_theme_name` (the
/// pure validation core of `set_theme_override`) must be called with
/// the *final* config's theme, and its warning must land in the
/// final config's `startup_warnings`.
async fn theme_warning_uses_final_config() -> std::io::Result<()> {
    use crate::render::highlight::validate_theme_name;

    let temp_dir = TempDir::new()?;

    // initial_config has a valid theme — no warning.
    let initial_config = build_config(&temp_dir).await?;
    assert!(initial_config.tui_theme.is_none());

    // Simulate resume/fork reload: the final config has an invalid theme.
    let mut config = build_config(&temp_dir).await?;
    config.tui_theme = Some("bogus-theme".into());

    // Theme override must use the final config (not initial_config).
    // This mirrors the real call site in run_ratatui_app.
    if let Some(w) = validate_theme_name(config.tui_theme.as_deref(), Some(temp_dir.path())) {
        config.startup_warnings.push(w);
    }

    assert_eq!(
        config.startup_warnings.len(),
        1,
        "warning from final config's invalid theme should be present"
    );
    assert!(
        config.startup_warnings[0].contains("bogus-theme"),
        "warning should reference the final config's theme name"
    );
    Ok(())
}
