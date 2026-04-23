#[derive(Debug, Clone)]
pub enum StatusAccountDisplay {
    ChatGpt {
        provider_label: String,
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey {
        provider_label: String,
    },
}
