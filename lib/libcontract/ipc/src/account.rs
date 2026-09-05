use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanType {
    #[default]
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Business,
    Enterprise,
    Edu,
    #[serde(other)]
    Unknown,
}
