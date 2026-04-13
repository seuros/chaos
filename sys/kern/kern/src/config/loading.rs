use std::path::Path;
use std::path::PathBuf;

use chaos_pwd::find_chaos_home;
use chaos_realpath::AbsolutePathBuf;
use toml::Value as TomlValue;

use super::ConfigOverrides;
use super::ConfigToml;
use crate::config::Config;
use crate::config::parsing::deserialize_config_toml_with_base as _deserialize_config_toml_with_base;
use crate::config::serialization;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::LoaderOverrides;
use crate::config_loader::load_config_layers_state;

/// Builder for constructing a [`Config`] from layered sources.
#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    pub(crate) chaos_home: Option<PathBuf>,
    pub(crate) cli_overrides: Option<Vec<(String, TomlValue)>>,
    pub(crate) harness_overrides: Option<ConfigOverrides>,
    pub(crate) loader_overrides: Option<LoaderOverrides>,
    pub(crate) fallback_cwd: Option<PathBuf>,
}

impl ConfigBuilder {
    pub fn chaos_home(mut self, chaos_home: PathBuf) -> Self {
        self.chaos_home = Some(chaos_home);
        self
    }

    pub fn cli_overrides(mut self, cli_overrides: Vec<(String, TomlValue)>) -> Self {
        self.cli_overrides = Some(cli_overrides);
        self
    }

    pub fn harness_overrides(mut self, harness_overrides: ConfigOverrides) -> Self {
        self.harness_overrides = Some(harness_overrides);
        self
    }

    pub fn loader_overrides(mut self, loader_overrides: LoaderOverrides) -> Self {
        self.loader_overrides = Some(loader_overrides);
        self
    }

    pub fn fallback_cwd(mut self, fallback_cwd: Option<PathBuf>) -> Self {
        self.fallback_cwd = fallback_cwd;
        self
    }

    pub async fn build(self) -> std::io::Result<Config> {
        let Self {
            chaos_home,
            cli_overrides,
            harness_overrides,
            loader_overrides,
            fallback_cwd,
        } = self;
        let chaos_home = chaos_home.map_or_else(find_chaos_home, std::io::Result::Ok)?;
        if let Err(err) = maybe_migrate_smart_approvals_alias(&chaos_home).await {
            tracing::warn!(error = %err, "failed to migrate smart_approvals feature alias");
        }
        let cli_overrides = cli_overrides.unwrap_or_default();
        let mut harness_overrides = harness_overrides.unwrap_or_default();
        let loader_overrides = loader_overrides.unwrap_or_default();
        let cwd_override = harness_overrides.cwd.as_deref().or(fallback_cwd.as_deref());
        let cwd = match cwd_override {
            Some(path) => AbsolutePathBuf::try_from(path)?,
            None => AbsolutePathBuf::current_dir()?,
        };
        harness_overrides.cwd = Some(cwd.to_path_buf());
        let config_layer_stack =
            load_config_layers_state(&chaos_home, Some(cwd), &cli_overrides, loader_overrides)
                .await?;
        let merged_toml = config_layer_stack.effective_config();

        let config_toml: ConfigToml = match merged_toml.try_into() {
            Ok(config_toml) => config_toml,
            Err(err) => {
                if let Some(config_error) =
                    crate::config_loader::first_layer_config_error(&config_layer_stack).await
                {
                    return Err(crate::config_loader::io_error_from_config_error(
                        std::io::ErrorKind::InvalidData,
                        config_error,
                        Some(err),
                    ));
                }
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
            }
        };
        Config::load_config_with_layer_stack(
            config_toml,
            harness_overrides,
            chaos_home,
            config_layer_stack,
        )
    }
}

async fn maybe_migrate_smart_approvals_alias(chaos_home: &Path) -> std::io::Result<bool> {
    serialization::maybe_migrate_smart_approvals_alias(chaos_home).await
}

/// Public load methods on [`Config`] that delegate to the builder or inner
/// loading path.
impl Config {
    /// This is the preferred way to create an instance of [Config].
    pub async fn load_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .build()
            .await
    }

    /// Load a default configuration when user config files are invalid.
    pub fn load_default_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        let chaos_home = find_chaos_home()?;
        let mut merged = toml::Value::try_from(ConfigToml::default()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to serialize default config: {e}"),
            )
        })?;
        let cli_layer = crate::config_loader::build_cli_overrides_layer(&cli_overrides);
        crate::config_loader::merge_toml_values(&mut merged, &cli_layer);
        let config_toml = _deserialize_config_toml_with_base(merged, &chaos_home)?;
        Self::load_config_with_layer_stack(
            config_toml,
            ConfigOverrides::default(),
            chaos_home,
            ConfigLayerStack::default(),
        )
    }

    /// Secondary load path for harnesses that need explicit overrides (e.g.
    /// `chaos exec` which always uses `ApprovalPolicy::Headless`).
    pub async fn load_with_cli_overrides_and_harness_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .harness_overrides(harness_overrides)
            .build()
            .await
    }
}
