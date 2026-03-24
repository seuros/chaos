#[derive(Debug, thiserror::Error)]
pub enum RationError {
    #[error("provider unreachable: {0}")]
    Unreachable(String),

    #[error("auth required: {0}")]
    AuthRequired(String),

    #[error("parse: {0}")]
    Parse(String),

    #[error("rate limited — ironic")]
    RateLimited,

    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}
