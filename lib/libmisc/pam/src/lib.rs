mod device_code_auth;
mod pkce;
mod server;

pub use codex_client::BuildCustomCaTransportError as BuildLoginHttpClientError;
pub use device_code_auth::DeviceCode;
pub use device_code_auth::complete_device_code_login;
pub use device_code_auth::request_device_code;
pub use device_code_auth::run_device_code_login;
pub use server::LoginServer;
pub use server::ServerOptions;
pub use server::ShutdownHandle;
pub use server::run_login_server;

// Re-export commonly used auth types and helpers from codex-core for compatibility
pub use chaos_kern::AuthManager;
pub use chaos_kern::CodexAuth;
pub use chaos_kern::auth::AuthDotJson;
pub use chaos_kern::auth::CLIENT_ID;
pub use chaos_kern::auth::CODEX_API_KEY_ENV_VAR;
pub use chaos_kern::auth::OPENAI_API_KEY_ENV_VAR;
pub use chaos_kern::auth::login_with_api_key;
pub use chaos_kern::auth::logout;
pub use chaos_kern::auth::save_auth;
pub use chaos_kern::token_data::TokenData;
pub use chaos_ipc::api::AuthMode;
