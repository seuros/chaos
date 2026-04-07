use super::LoaderOverrides;
use chaos_realpath::AbsolutePathBuf;
use chaos_sysctl::config_error_from_toml;
use chaos_sysctl::io_error_from_config_error;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;
use toml::Value as TomlValue;

const CODEX_MANAGED_CONFIG_SYSTEM_PATH: &str = "/etc/chaos/managed_config.toml";

#[derive(Debug, Clone)]
pub(super) struct MangedConfigFromFile {
    pub managed_config: TomlValue,
    pub file: AbsolutePathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct LoadedConfigLayers {
    /// If present, data read from a file such as `/etc/chaos/managed_config.toml`.
    pub managed_config: Option<MangedConfigFromFile>,
}

pub(super) async fn load_config_layers_internal(
    chaos_home: &Path,
    overrides: LoaderOverrides,
) -> io::Result<LoadedConfigLayers> {
    let LoaderOverrides {
        managed_config_path,
        ..
    } = overrides;

    let managed_config_path = AbsolutePathBuf::from_absolute_path(
        managed_config_path.unwrap_or_else(|| managed_config_default_path(chaos_home)),
    )?;

    let managed_config =
        read_config_from_path(&managed_config_path, /*log_missing_as_info*/ false)
            .await?
            .map(|managed_config| MangedConfigFromFile {
                managed_config,
                file: managed_config_path.clone(),
            });

    Ok(LoadedConfigLayers { managed_config })
}

pub(super) async fn read_config_from_path(
    path: impl AsRef<Path>,
    log_missing_as_info: bool,
) -> io::Result<Option<TomlValue>> {
    match fs::read_to_string(path.as_ref()).await {
        Ok(contents) => match toml::from_str::<TomlValue>(&contents) {
            Ok(value) => Ok(Some(value)),
            Err(err) => {
                tracing::error!("Failed to parse {}: {err}", path.as_ref().display());
                let config_error = config_error_from_toml(path.as_ref(), &contents, err.clone());
                Err(io_error_from_config_error(
                    io::ErrorKind::InvalidData,
                    config_error,
                    Some(err),
                ))
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if log_missing_as_info {
                tracing::info!("{} not found, using defaults", path.as_ref().display());
            } else {
                tracing::debug!("{} not found", path.as_ref().display());
            }
            Ok(None)
        }
        Err(err) => {
            tracing::error!("Failed to read {}: {err}", path.as_ref().display());
            Err(err)
        }
    }
}

/// Return the default managed config path.
pub(super) fn managed_config_default_path(chaos_home: &Path) -> PathBuf {
    let _ = chaos_home;
    PathBuf::from(CODEX_MANAGED_CONFIG_SYSTEM_PATH)
}
