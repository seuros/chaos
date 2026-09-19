use anyhow::Context as _;
use chaos_getopt::CliConfigOverrides;
use chaos_kern::AuthManager;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::reflex::diagnostics;

#[derive(Debug, usage::Args)]
pub struct ReflexCommand {
    #[usage(subcommand)]
    pub command: ReflexSubcommand,
}

#[derive(Debug, usage::Subcommands)]
pub enum ReflexSubcommand {
    /// Test saved Jev credentials with a synthetic read-only action (no files or chat sent).
    Test,
}

pub async fn run(
    command: ReflexCommand,
    config_overrides: CliConfigOverrides,
    config_profile: Option<String>,
) -> anyhow::Result<()> {
    let config = ConfigBuilder::default()
        .cli_overrides(
            config_overrides
                .parse_overrides()
                .map_err(anyhow::Error::msg)?,
        )
        .harness_overrides(ConfigOverrides {
            config_profile,
            ..Default::default()
        })
        .build()
        .await
        .context("failed to load config")?;
    let auth = AuthManager::shared(
        config.chaos_home.clone(),
        false,
        config.cli_auth_credentials_store_mode,
    );
    match command.command {
        ReflexSubcommand::Test => println!("{}", diagnostics::test(&config, Some(&auth)).await?),
    }
    Ok(())
}
