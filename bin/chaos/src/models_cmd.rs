use anyhow::Context as _;
use chaos_ipc::openai_models::ModelPreset;
use chaos_kern::AuthManager;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_kern::models_manager::manager::ModelsManager;
use chaos_kern::models_manager::manager::RefreshStrategy;
use chaos_pwd::find_chaos_home;
use clap::Args;

#[derive(Debug, Args)]
pub struct ModelsCli {
    /// Override the provider (e.g. openai, anthropic, or a key from [model_providers]).
    #[arg(long)]
    pub provider: Option<String>,

    /// Force a fresh fetch from the provider, ignoring the local cache.
    #[arg(long, default_value_t = false)]
    pub refresh: bool,
}

pub async fn run(cli: ModelsCli, config_profile: Option<String>) -> anyhow::Result<()> {
    let chaos_home = find_chaos_home().context("could not locate chaos home directory")?;

    let overrides = ConfigOverrides {
        model_provider: cli.provider,
        config_profile,
        ..ConfigOverrides::default()
    };

    let config = ConfigBuilder::default()
        .harness_overrides(overrides)
        .build()
        .await
        .context("failed to load config")?;

    let auth_manager = AuthManager::shared(
        config.chaos_home.clone(),
        true,
        config.cli_auth_credentials_store_mode,
    );

    let models_manager = ModelsManager::new_with_provider(
        chaos_home,
        auth_manager,
        config.model_catalog.clone(),
        CollaborationModesConfig::default(),
        config.model_provider.clone(),
    );

    let strategy = if cli.refresh {
        RefreshStrategy::Online
    } else {
        RefreshStrategy::OnlineIfUncached
    };

    let models = models_manager.list_models(strategy).await;
    print_models(&models, &config.model_provider.name);
    Ok(())
}

fn print_models(models: &[ModelPreset], provider: &str) {
    if models.is_empty() {
        println!("No models available for provider '{provider}'.");
        return;
    }
    println!("Models for provider '{provider}':\n");
    for m in models {
        let default_marker = if m.is_default { " (default)" } else { "" };
        println!("  {}{}", m.model, default_marker);
        if !m.display_name.is_empty() && m.display_name != m.model {
            println!("    {}", m.display_name);
        }
    }
}
