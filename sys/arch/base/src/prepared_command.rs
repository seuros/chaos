use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::models::PermissionProfile;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_pf::NetworkProxy;

/// Portable input accepted by the platform sandbox selected at compile time.
pub struct SandboxRequest<'a> {
    pub executable: &'a Path,
    pub command: Vec<String>,
    pub file_system_policy: &'a VfsPolicy,
    pub network_policy: SocketPolicy,
    pub sandbox_policy_cwd: &'a Path,
    pub enforce_managed_network: bool,
    pub network: Option<&'a NetworkProxy>,
    pub platform_permissions: Option<&'a PermissionProfile>,
}

/// Concrete command produced by the active platform sandbox implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub arg0: Option<String>,
}
