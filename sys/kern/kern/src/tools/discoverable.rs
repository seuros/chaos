use chaos_ipc::api::AppInfo;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiscoverableToolType {
    Connector,
}

impl DiscoverableToolType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Connector => "connector",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiscoverableToolAction {
    Install,
    Enable,
}

impl DiscoverableToolAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Enable => "enable",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DiscoverableTool {
    Connector(Box<AppInfo>),
}

impl DiscoverableTool {
    pub(crate) fn tool_type(&self) -> DiscoverableToolType {
        match self {
            Self::Connector(_) => DiscoverableToolType::Connector,
        }
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Connector(connector) => connector.id.as_str(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Connector(connector) => connector.name.as_str(),
        }
    }

    pub(crate) fn description(&self) -> Option<&str> {
        match self {
            Self::Connector(connector) => connector.description.as_deref(),
        }
    }
}

impl From<AppInfo> for DiscoverableTool {
    fn from(value: AppInfo) -> Self {
        Self::Connector(Box::new(value))
    }
}
