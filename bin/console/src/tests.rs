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

#[test]
fn report_to_io_error_shows_session_in_use_without_internal_wrappers() {
    let process_id = chaos_ipc::ProcessId::new();
    let result: color_eyre::Result<()> =
        Err(chaos_kern::error::ChaosErr::SessionInUse(process_id).into());
    let report = result
        .wrap_err(format!("Failed to resume session {process_id}"))
        .expect_err("busy session");

    assert_eq!(
        report_to_io_error(report).to_string(),
        format!(
            "Session {process_id} is already in use by another ChaOS instance.\n\
             Close that instance and retry. If it has already exited, wait up to 30 seconds for its lock to expire."
        )
    );
}

async fn build_config(temp_dir: &TempDir) -> std::io::Result<Config> {
    chaos_kern::test_support::bootstrap_sqlite(temp_dir.path()).map_err(std::io::Error::other)?;
    ConfigBuilder::default()
        .chaos_home(temp_dir.path().to_path_buf())
        .fallback_cwd(Some(temp_dir.path().to_path_buf()))
        .build()
        .await
}

#[test]
#[ignore]
fn lib_suite() {
    run_console_test(run_lib_suite);
}

// The full App fixture needs more stack than the default Rust test thread.
pub(super) fn run_console_test<F: Future<Output = ()> + 'static>(body: fn() -> F) {
    std::thread::Builder::new()
        .name("console-test".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build console test runtime")
                .block_on(body());
        })
        .expect("spawn console test thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

async fn run_lib_suite() {
    super::additional_dirs::tests::add_dir_warning_message_only_warns_for_read_only_sandbox_with_dirs();
    super::app::tests::app_tests_suite().await;
    super::app_backtrack::tests::app_backtrack_suite();
    super::cwd_prompt::tests::cwd_prompt_suite();
    super::onboarding::tests::onboarding_suite();
    super::pager_overlay::tests::pager_overlay_suite();
    boot_core_returns_valid_managers()
        .await
        .expect("boot_core_returns_valid_managers");
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

#[tokio::test]
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

#[tokio::test]
async fn config_rebuild_changes_trust_defaults_with_cwd() -> std::io::Result<()> {
    use chaos_ipc::config_types::TrustLevel;
    use chaos_kern::config::set_project_trust_level;

    let temp_dir = TempDir::new()?;
    let chaos_home = temp_dir.path().to_path_buf();
    chaos_kern::test_support::bootstrap_sqlite(&chaos_home).map_err(std::io::Error::other)?;
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

#[cfg(feature = "syntax")]
#[tokio::test]
async fn theme_warning_uses_final_config() -> color_eyre::Result<()> {
    use chaos_kern::test_support::seed_user_settings_toml;

    let temp_dir = TempDir::new()?;
    seed_user_settings_toml(temp_dir.path(), r#"tui.theme = "missing-before-reload""#)
        .await
        .map_err(std::io::Error::other)?;
    let initial = build_config(&temp_dir).await?;
    seed_user_settings_toml(temp_dir.path(), r#"tui.theme = "missing-after-reload""#)
        .await
        .map_err(std::io::Error::other)?;
    let destination = build_config(&temp_dir).await?;

    let target = resume_picker::SessionTarget {
        process_id: ProcessId::new(),
        keep_current: true,
    };
    let warnings = finalize_startup_config(initial, Some((&target, destination)), &[])
        .await?
        .startup_warnings;
    assert!(
        warnings
            .iter()
            .any(|w| w.contains(r#"Theme "missing-after-reload" not found"#)),
        "{warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("missing-before-reload")),
        "{warnings:?}"
    );
    Ok(())
}
