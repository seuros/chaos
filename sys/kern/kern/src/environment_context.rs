use crate::chaos::TurnContext;
use crate::contextual_user_message::ENVIRONMENT_CONTEXT_FRAGMENT;
use crate::shell::Shell;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::TurnContextNetworkItem;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

mod platform;
mod xml;

use platform::PlatformContext;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "environment_context", rename_all = "snake_case")]
pub(crate) struct EnvironmentContext {
    #[serde(default)]
    pub platform: Option<PlatformContext>,
    pub cwd: Option<PathBuf>,
    pub shell: Shell,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
    pub network: Option<NetworkContext>,
    pub subagents: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct NetworkContext {
    allowed_domains: Vec<String>,
    denied_domains: Vec<String>,
}

impl EnvironmentContext {
    pub fn new(
        cwd: Option<PathBuf>,
        shell: Shell,
        current_date: Option<String>,
        timezone: Option<String>,
        network: Option<NetworkContext>,
        subagents: Option<String>,
    ) -> Self {
        Self {
            platform: None,
            cwd,
            shell,
            current_date,
            timezone,
            network,
            subagents,
        }
    }

    /// Compares two environment contexts, ignoring the shell. Useful when
    /// comparing turn to turn, since the initial environment_context will
    /// include the shell, and then it is not configurable from turn to turn.
    pub fn equals_except_shell(&self, other: &EnvironmentContext) -> bool {
        let EnvironmentContext {
            platform,
            cwd,
            current_date,
            timezone,
            network,
            subagents,
            shell: _,
        } = other;
        self.platform == *platform
            && self.cwd == *cwd
            && self.current_date == *current_date
            && self.timezone == *timezone
            && self.network == *network
            && self.subagents == *subagents
    }

    pub fn diff_from_turn_context_item(
        before: &TurnContextItem,
        after: &TurnContext,
        shell: &Shell,
    ) -> Self {
        let before_network = Self::network_from_turn_context_item(before);
        let after_network = Self::network_from_turn_context(after);
        let cwd = if before.cwd != after.cwd {
            Some(after.cwd.clone())
        } else {
            None
        };
        let current_date = after.current_date.clone();
        let timezone = after.timezone.clone();
        let network = if before_network != after_network {
            after_network
        } else {
            before_network
        };
        EnvironmentContext::new(
            cwd,
            shell.clone(),
            current_date,
            timezone,
            network,
            /*subagents*/ None,
        )
    }

    pub fn from_turn_context(turn_context: &TurnContext, shell: &Shell) -> Self {
        let mut context = Self::new(
            Some(turn_context.cwd.clone()),
            shell.clone(),
            turn_context.current_date.clone(),
            turn_context.timezone.clone(),
            Self::network_from_turn_context(turn_context),
            /*subagents*/ None,
        );
        context.platform = Some(PlatformContext::local());
        context
    }

    pub fn from_turn_context_item(turn_context_item: &TurnContextItem, shell: &Shell) -> Self {
        let mut context = Self::new(
            Some(turn_context_item.cwd.clone()),
            shell.clone(),
            turn_context_item.current_date.clone(),
            turn_context_item.timezone.clone(),
            Self::network_from_turn_context_item(turn_context_item),
            /*subagents*/ None,
        );
        context.platform = Some(PlatformContext::local());
        context
    }

    pub fn with_subagents(mut self, subagents: String) -> Self {
        if !subagents.is_empty() {
            self.subagents = Some(subagents);
        }
        self
    }

    fn network_from_turn_context(turn_context: &TurnContext) -> Option<NetworkContext> {
        let network = turn_context
            .config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()?;

        Some(NetworkContext {
            allowed_domains: network.allowed_domains.clone().unwrap_or_default(),
            denied_domains: network.denied_domains.clone().unwrap_or_default(),
        })
    }

    fn network_from_turn_context_item(
        turn_context_item: &TurnContextItem,
    ) -> Option<NetworkContext> {
        let TurnContextNetworkItem {
            allowed_domains,
            denied_domains,
        } = turn_context_item.network.as_ref()?;
        Some(NetworkContext {
            allowed_domains: allowed_domains.clone(),
            denied_domains: denied_domains.clone(),
        })
    }
}

impl From<EnvironmentContext> for ResponseItem {
    fn from(ec: EnvironmentContext) -> Self {
        ENVIRONMENT_CONTEXT_FRAGMENT.into_message(ec.serialize_to_xml())
    }
}

#[cfg(test)]
#[path = "environment_context_tests.rs"]
mod tests;
