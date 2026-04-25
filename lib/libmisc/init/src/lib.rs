//! `chaos-coreboot` — the shared userland initialisation step for a chaos frontend.
//!
//! Given a fully-loaded [`Config`], produce the runtime pair every frontend
//! needs before it can open a `chaos_session::ClientSession`: a shared
//! [`AuthManager`] and a [`ProcessTable`] wired to that auth manager.
//!
//! Think of it as the chaos equivalent of PID 1: the first thing a userland
//! binary runs after config parsing, responsible for wiring up the long-lived
//! runtime objects that every subsequent operation depends on.
//!
//! This crate is intentionally tiny. Config loading is frontend-specific
//! (CLI parsing, onboarding flows, env overrides) and stays in each binary.
//! This crate only covers the shared "given a config, wire the managers"
//! step, so `bin/console`, `bin/xclient`, and any future frontend can stand
//! on the same initialisation code.
//!
//! # Example
//!
//! ```no_run
//! use chaos_coreboot::CoreBoot;
//! use chaos_ipc::protocol::SessionSource;
//! use chaos_kern::config::Config;
//! use chaos_kern::models_manager::CollaborationModesConfig;
//!
//! # fn demo(config: Config) {
//! let coreboot = CoreBoot::boot(
//!     &config,
//!     SessionSource::Cli,
//!     CollaborationModesConfig { default_mode_request_user_input: true },
//! );
//! // coreboot.auth_manager and coreboot.process_table are now ready to hand to
//! // chaos_session::ClientSession::spawn.
//! # }
//! ```

use std::sync::Arc;

use chaos_ipc::protocol::SessionSource;
use chaos_kern::AuthManager;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_kern::models_manager::CollaborationModesConfig;

/// The runtime pair every chaos frontend needs to open a session.
///
/// Both fields are shared [`Arc`] handles: the auth manager is shared so
/// every [`chaos_kern::Process`] spawned from the table can refresh
/// credentials against a single source of truth, and the process table is
/// shared so forks, resumes, and the initial boot all go through the same
/// process registry.
pub struct CoreBoot {
    /// Shared auth manager. Hand this to any frontend component that needs
    /// to read or refresh credentials (onboarding flows, status bars,
    /// rollout recorders).
    pub auth_manager: Arc<AuthManager>,
    /// Shared process table. Hand this to
    /// `chaos_session::ClientSession::spawn` (via `chaos-session`) to
    /// cold-start a kernel session, or use it directly for fork/resume
    /// flows that want a specific existing [`chaos_kern::Process`].
    pub process_table: Arc<ProcessTable>,
}

impl CoreBoot {
    /// Wire an [`AuthManager`] and a [`ProcessTable`] for the given config.
    ///
    /// * `config` — a fully-loaded kernel config. The caller owns config
    ///   loading (CLI parsing, `-c` overrides, onboarding prompts). This
    ///   helper only reads it.
    /// * `session_source` — who is booting. Typically
    ///   [`SessionSource::Cli`] for `bin/console`, but
    ///   [`SessionSource::VSCode`], [`SessionSource::Exec`], etc. exist for
    ///   other surfaces.
    /// * `collab_modes` — collaboration-mode preset wiring. A GUI might
    ///   hard-wire a different default than a TUI.
    ///
    /// # Notes
    ///
    /// [`AuthManager::shared`] is called with `enable_codex_api_key_env =
    /// false`; frontends that want the legacy env-var path must call
    /// [`AuthManager::shared`] themselves instead of using this helper.
    pub fn boot(
        config: &Config,
        session_source: SessionSource,
        collab_modes: CollaborationModesConfig,
    ) -> Self {
        let auth_manager = AuthManager::shared(
            config.chaos_home.clone(),
            false, // enable_codex_api_key_env
            config.cli_auth_credentials_store_mode,
        );
        let process_table = Arc::new(ProcessTable::new(
            config,
            auth_manager.clone(),
            session_source,
            collab_modes,
        ));
        Self {
            auth_manager,
            process_table,
        }
    }
}
