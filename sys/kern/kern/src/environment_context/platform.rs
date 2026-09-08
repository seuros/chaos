use std::sync::LazyLock;

use serde::Deserialize;
use serde::Serialize;

static LOCAL_PLATFORM: LazyLock<PlatformContext> = LazyLock::new(PlatformContext::detect);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlatformContext {
    pub os: String,
    pub os_version: Option<String>,
    pub kernel: String,
    pub kernel_release: Option<String>,
    pub arch: String,
    pub distribution: Option<DistributionContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DistributionContext {
    pub name: String,
    pub version: Option<String>,
}

impl PlatformContext {
    pub(super) fn local() -> Self {
        LOCAL_PLATFORM.clone()
    }

    fn detect() -> Self {
        let system = chaos_sysinfo::sysinfo();
        let release = os_info::get();
        let version = match release.version() {
            os_info::Version::Unknown => None,
            version => known(&version.to_string()),
        };
        let distribution = (system.os == "linux")
            .then(|| {
                known(&system.os_distro).map(|name| DistributionContext {
                    name,
                    version: version.clone(),
                })
            })
            .flatten();
        Self {
            os: system.os.clone(),
            os_version: (system.os != "linux").then_some(version).flatten(),
            kernel: match system.os.as_str() {
                "macos" => "darwin".to_string(),
                os => os.to_string(),
            },
            kernel_release: known(&system.os_version),
            arch: system.arch.clone(),
            distribution,
        }
    }
}

fn known(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("unknown")).then(|| value.to_string())
}
