use crate::exec::SandboxType;
use crate::protocol::SandboxPolicy;
use crate::safety::get_platform_sandbox;

pub(crate) fn sandbox_tag(policy: &SandboxPolicy) -> &'static str {
    if matches!(policy, SandboxPolicy::RootAccess) {
        return "none";
    }
    if matches!(policy, SandboxPolicy::ExternalSandbox { .. }) {
        return "external";
    }

    get_platform_sandbox()
        .map(SandboxType::as_metric_tag)
        .unwrap_or("none")
}

#[cfg(test)]
#[path = "sandbox_tags_tests.rs"]
mod tests;
