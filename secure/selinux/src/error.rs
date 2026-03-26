use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: TextPosition,
    pub end: TextPosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLocation {
    pub path: String,
    pub range: TextRange,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid decision: {0}")]
    InvalidDecision(String),
    #[error("invalid pattern element: {0}")]
    InvalidPattern(String),
    #[error("invalid example: {0}")]
    InvalidExample(String),
    #[error("invalid rule: {0}")]
    InvalidRule(String),
    #[error(
        "expected every example to match at least one rule. rules: {rules:?}; unmatched examples: \
         {examples:?}"
    )]
    ExampleDidNotMatch {
        rules: Vec<String>,
        examples: Vec<String>,
        location: Option<ErrorLocation>,
    },
    #[error("expected example to not match rule `{rule}`: {example}")]
    ExampleDidMatch {
        rule: String,
        example: String,
        location: Option<ErrorLocation>,
    },
    #[error("lua error: {0}")]
    Lua(String),
}

impl Error {
    pub fn with_location(self, location: ErrorLocation) -> Self {
        match self {
            Error::ExampleDidNotMatch {
                rules,
                examples,
                location: None,
            } => Error::ExampleDidNotMatch {
                rules,
                examples,
                location: Some(location),
            },
            Error::ExampleDidMatch {
                rule,
                example,
                location: None,
            } => Error::ExampleDidMatch {
                rule,
                example,
                location: Some(location),
            },
            other => other,
        }
    }

    pub fn location(&self) -> Option<ErrorLocation> {
        match self {
            Error::ExampleDidNotMatch { location, .. }
            | Error::ExampleDidMatch { location, .. } => location.clone(),
            Error::Lua(msg) => parse_lua_error_location(msg),
            _ => None,
        }
    }
}

/// Try to extract file:line from a Lua error message.
/// Lua errors typically look like: `[string "test.rules"]:3: some error`
fn parse_lua_error_location(message: &str) -> Option<ErrorLocation> {
    let first_line = message.lines().next()?.trim();

    // Pattern: [string "FILENAME"]:LINE: ...
    let rest = first_line.strip_prefix("[string \"")?;
    let (path, rest) = rest.split_once("\"]:")?;
    let (line_str, _) = rest.split_once(':')?;
    let line = line_str.trim().parse::<usize>().ok()?;

    if line == 0 {
        return None;
    }

    Some(ErrorLocation {
        path: path.to_string(),
        range: TextRange {
            start: TextPosition { line, column: 1 },
            end: TextPosition { line, column: 1 },
        },
    })
}
