use std::collections::BTreeMap;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsEntry;
use chaos_ipc::permissions::VfsPath;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsSpecialPath;
use chaos_pf::NetworkMode;
use chaos_pf::NetworkProxyConfig;
use chaos_realpath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
pub struct PermissionsToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, PermissionProfileToml>,
}

impl PermissionsToml {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PermissionProfileToml {
    pub filesystem: Option<FilesystemPermissionsToml>,
    pub network: Option<NetworkToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
pub struct FilesystemPermissionsToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, FilesystemPermissionToml>,
}

impl FilesystemPermissionsToml {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(untagged)]
pub enum FilesystemPermissionToml {
    Access(VfsAccessMode),
    Scoped(BTreeMap<String, VfsAccessMode>),
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NetworkToml {
    pub enabled: Option<bool>,
    pub proxy_url: Option<String>,
    pub enable_socks5: Option<bool>,
    pub socks_url: Option<String>,
    pub enable_socks5_udp: Option<bool>,
    pub allow_upstream_proxy: Option<bool>,
    pub dangerously_allow_non_loopback_proxy: Option<bool>,
    pub dangerously_allow_all_unix_sockets: Option<bool>,
    #[schemars(with = "Option<NetworkModeSchema>")]
    pub mode: Option<NetworkMode>,
    pub allowed_domains: Option<Vec<String>>,
    pub denied_domains: Option<Vec<String>>,
    pub allow_unix_sockets: Option<Vec<String>>,
    pub allow_local_binding: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum NetworkModeSchema {
    Limited,
    Full,
}

impl NetworkToml {
    pub fn apply_to_network_proxy_config(&self, config: &mut NetworkProxyConfig) {
        if let Some(enabled) = self.enabled {
            config.network.enabled = enabled;
        }
        if let Some(proxy_url) = self.proxy_url.as_ref() {
            config.network.proxy_url = proxy_url.clone();
        }
        if let Some(enable_socks5) = self.enable_socks5 {
            config.network.enable_socks5 = enable_socks5;
        }
        if let Some(socks_url) = self.socks_url.as_ref() {
            config.network.socks_url = socks_url.clone();
        }
        if let Some(enable_socks5_udp) = self.enable_socks5_udp {
            config.network.enable_socks5_udp = enable_socks5_udp;
        }
        if let Some(allow_upstream_proxy) = self.allow_upstream_proxy {
            config.network.allow_upstream_proxy = allow_upstream_proxy;
        }
        if let Some(dangerously_allow_non_loopback_proxy) =
            self.dangerously_allow_non_loopback_proxy
        {
            config.network.dangerously_allow_non_loopback_proxy =
                dangerously_allow_non_loopback_proxy;
        }
        if let Some(dangerously_allow_all_unix_sockets) = self.dangerously_allow_all_unix_sockets {
            config.network.dangerously_allow_all_unix_sockets = dangerously_allow_all_unix_sockets;
        }
        if let Some(mode) = self.mode {
            config.network.mode = mode;
        }
        if let Some(allowed_domains) = self.allowed_domains.as_ref() {
            config.network.allowed_domains = allowed_domains.clone();
        }
        if let Some(denied_domains) = self.denied_domains.as_ref() {
            config.network.denied_domains = denied_domains.clone();
        }
        if let Some(allow_unix_sockets) = self.allow_unix_sockets.as_ref() {
            config.network.allow_unix_sockets = allow_unix_sockets.clone();
        }
        if let Some(allow_local_binding) = self.allow_local_binding {
            config.network.allow_local_binding = allow_local_binding;
        }
    }

    pub fn to_network_proxy_config(&self) -> NetworkProxyConfig {
        let mut config = NetworkProxyConfig::default();
        self.apply_to_network_proxy_config(&mut config);
        config
    }
}

pub fn network_proxy_config_from_profile_network(
    network: Option<&NetworkToml>,
) -> NetworkProxyConfig {
    network.map_or_else(
        NetworkProxyConfig::default,
        NetworkToml::to_network_proxy_config,
    )
}

pub fn resolve_permission_profile<'a>(
    permissions: &'a PermissionsToml,
    profile_name: &str,
) -> io::Result<&'a PermissionProfileToml> {
    permissions.entries.get(profile_name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("default_permissions refers to undefined profile `{profile_name}`"),
        )
    })
}

pub fn compile_permission_profile(
    permissions: &PermissionsToml,
    profile_name: &str,
    startup_warnings: &mut Vec<String>,
) -> io::Result<(VfsPolicy, SocketPolicy)> {
    let profile = resolve_permission_profile(permissions, profile_name)?;

    let mut entries = Vec::new();
    if let Some(filesystem) = profile.filesystem.as_ref() {
        if filesystem.is_empty() {
            push_warning(
                startup_warnings,
                missing_filesystem_entries_warning(profile_name),
            );
        } else {
            for (path, permission) in &filesystem.entries {
                compile_filesystem_permission(path, permission, &mut entries, startup_warnings)?;
            }
        }
    } else {
        push_warning(
            startup_warnings,
            missing_filesystem_entries_warning(profile_name),
        );
    }

    let socket_policy = compile_socket_policy(profile.network.as_ref());

    Ok((VfsPolicy::restricted(entries), socket_policy))
}

fn compile_socket_policy(network: Option<&NetworkToml>) -> SocketPolicy {
    let Some(network) = network else {
        return SocketPolicy::Restricted;
    };

    match network.enabled {
        Some(true) => SocketPolicy::Enabled,
        _ => SocketPolicy::Restricted,
    }
}

fn compile_filesystem_permission(
    path: &str,
    permission: &FilesystemPermissionToml,
    entries: &mut Vec<VfsEntry>,
    startup_warnings: &mut Vec<String>,
) -> io::Result<()> {
    match permission {
        FilesystemPermissionToml::Access(access) => entries.push(VfsEntry {
            path: compile_filesystem_path(path, startup_warnings)?,
            access: *access,
        }),
        FilesystemPermissionToml::Scoped(scoped_entries) => {
            for (subpath, access) in scoped_entries {
                entries.push(VfsEntry {
                    path: compile_scoped_filesystem_path(path, subpath, startup_warnings)?,
                    access: *access,
                });
            }
        }
    }
    Ok(())
}

fn compile_filesystem_path(path: &str, startup_warnings: &mut Vec<String>) -> io::Result<VfsPath> {
    if let Some(special) = parse_special_path(path) {
        maybe_push_unknown_special_path_warning(&special, startup_warnings);
        return Ok(VfsPath::Special { value: special });
    }

    let path = parse_absolute_path(path)?;
    Ok(VfsPath::Path { path })
}

fn compile_scoped_filesystem_path(
    path: &str,
    subpath: &str,
    startup_warnings: &mut Vec<String>,
) -> io::Result<VfsPath> {
    if subpath == "." {
        return compile_filesystem_path(path, startup_warnings);
    }

    if let Some(special) = parse_special_path(path) {
        let subpath = parse_relative_subpath(subpath)?;
        let special = match special {
            VfsSpecialPath::ProjectRoots { .. } => Ok(VfsPath::Special {
                value: VfsSpecialPath::project_roots(Some(subpath)),
            }),
            VfsSpecialPath::Unknown { path, .. } => Ok(VfsPath::Special {
                value: VfsSpecialPath::unknown(path, Some(subpath)),
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("filesystem path `{path}` does not support nested entries"),
            )),
        }?;
        if let VfsPath::Special { value } = &special {
            maybe_push_unknown_special_path_warning(value, startup_warnings);
        }
        return Ok(special);
    }

    let subpath = parse_relative_subpath(subpath)?;
    let base = parse_absolute_path(path)?;
    let path = AbsolutePathBuf::resolve_path_against_base(&subpath, base.as_path())?;
    Ok(VfsPath::Path { path })
}

// WARNING: keep this parser forward-compatible.
// Adding a new `:special_path` must not make older Chaos versions reject the
// config. Unknown values intentionally round-trip through
// `VfsSpecialPath::Unknown` so they can be surfaced as warnings and
// ignored, rather than aborting config load.
fn parse_special_path(path: &str) -> Option<VfsSpecialPath> {
    match path {
        ":root" => Some(VfsSpecialPath::Root),
        ":minimal" => Some(VfsSpecialPath::Minimal),
        ":project_roots" => Some(VfsSpecialPath::project_roots(/*subpath*/ None)),
        ":tmpdir" => Some(VfsSpecialPath::Tmpdir),
        _ if path.starts_with(':') => {
            Some(VfsSpecialPath::unknown(path, /*subpath*/ None))
        }
        _ => None,
    }
}

fn parse_absolute_path(path: &str) -> io::Result<AbsolutePathBuf> {
    let path_ref = Path::new(path);
    if !path_ref.is_absolute() && path != "~" && !path.starts_with("~/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem path `{path}` must be absolute, use `~/...`, or start with `:`"),
        ));
    }
    AbsolutePathBuf::from_absolute_path(path_ref)
}

fn parse_relative_subpath(subpath: &str) -> io::Result<PathBuf> {
    let path = Path::new(subpath);
    if !subpath.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Ok(path.to_path_buf());
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "filesystem subpath `{}` must be a descendant path without `.` or `..` components",
            path.display()
        ),
    ))
}

fn push_warning(startup_warnings: &mut Vec<String>, message: String) {
    tracing::warn!("{message}");
    startup_warnings.push(message);
}

fn missing_filesystem_entries_warning(profile_name: &str) -> String {
    format!(
        "Permissions profile `{profile_name}` does not define any recognized filesystem entries for this version of Chaos. Filesystem access will remain restricted. Upgrade Chaos if this profile expects filesystem permissions."
    )
}

fn maybe_push_unknown_special_path_warning(
    special: &VfsSpecialPath,
    startup_warnings: &mut Vec<String>,
) {
    let VfsSpecialPath::Unknown { path, subpath } = special else {
        return;
    };
    push_warning(
        startup_warnings,
        match subpath.as_deref() {
            Some(subpath) => format!(
                "Configured filesystem path `{path}` with nested entry `{}` is not recognized by this version of Chaos and will be ignored. Upgrade Chaos if this path is required.",
                subpath.display()
            ),
            None => format!(
                "Configured filesystem path `{path}` is not recognized by this version of Chaos and will be ignored. Upgrade Chaos if this path is required."
            ),
        },
    );
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
