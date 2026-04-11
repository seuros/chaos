//! OpenAI-owned endpoints.
//!
//! **Do not rename the `codex` path segment.** It is OpenAI's product
//! namespace on their own infrastructure — our internal refactors have no
//! authority over it. Renaming these locally produces 404s against
//! production.

// ---------------------------------------------------------------------------
// Direct OpenAI API (API-key auth path)
// ---------------------------------------------------------------------------

/// Base URL for the direct OpenAI API, used when the user authenticates
/// with an API key rather than a ChatGPT subscription. All of OpenAI's
/// public endpoints (`/chat/completions`, `/responses`, `/embeddings`, …)
/// hang off this root.
pub const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

// ---------------------------------------------------------------------------
// ChatGPT backend API (subscription-auth path)
// ---------------------------------------------------------------------------

/// Path-only component of the ChatGPT backend API (without scheme or host).
/// Useful for test fixtures that mount a local mock server and need to
/// mimic the real URL shape.
pub const CHATGPT_BACKEND_PATH: &str = "/backend-api/codex";

/// Base URL for the ChatGPT backend API used when the user signs in with a
/// ChatGPT subscription (Plus / Pro / Team) instead of an API key. Requests
/// are proxied through chatgpt.com and billed against the plan quota.
pub const CHATGPT_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/codex";

/// OpenAI **Responses API** endpoint reached via the ChatGPT subscription
/// proxy. Stateful successor to Chat Completions; `gpt-5`-class models only
/// speak this protocol.
pub const CHATGPT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Model-list endpoint reached via the ChatGPT subscription proxy. Returns
/// the set of models the current session is entitled to. Populates
/// `models_cache.json` on disk.
pub const CHATGPT_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

// ---------------------------------------------------------------------------
// ChatGPT web surface (user-facing links, not APIs)
// ---------------------------------------------------------------------------

/// Path component appended to the auth issuer for the device-code entry
/// point. Combined with the issuer origin at call sites (see
/// [RFC 8628](https://datatracker.ietf.org/doc/html/rfc8628) device flow).
pub const CHATGPT_DEVICE_AUTH_PATH: &str = "/codex/device";

// ---------------------------------------------------------------------------
// Developer documentation
// ---------------------------------------------------------------------------

/// MCP integration documentation — shown from the empty-state of the
/// TUI `/mcp` panel.
pub const DEVELOPERS_MCP_DOCS: &str = "https://developers.openai.com/codex/mcp";
