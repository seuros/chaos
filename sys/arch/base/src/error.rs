//! Error types for the Alcatraz sandbox subsystem.
//!
//! These live in the base crate so OS-specific implementations (landlock,
//! capsicum, seatbelt, pledge) can return them without depending on chaos-kern.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AlcatrazError>;

#[derive(Error, Debug)]
pub enum AlcatrazError {
    /// Landlock was unable to fully enforce all sandbox rules.
    #[error("landlock was not able to fully enforce all sandbox rules")]
    LandlockRestrict,

    /// Seccomp filter installation failed.
    #[cfg(target_os = "linux")]
    #[error("seccomp setup error")]
    SeccompInstall(#[from] seccompiler::Error),

    /// Seccomp backend error.
    #[cfg(target_os = "linux")]
    #[error("seccomp backend error")]
    SeccompBackend(#[from] seccompiler::BackendError),

    /// Landlock ruleset error.
    #[cfg(target_os = "linux")]
    #[error("landlock ruleset error")]
    LandlockRuleset(#[from] landlock::RulesetError),

    /// Capsicum cap_enter() or cap_rights_limit() failed.
    #[cfg(target_os = "freebsd")]
    #[error("capsicum capability mode error: {0}")]
    CapsicumRestrict(String),

    /// The requested operation is not supported on this platform.
    #[error("{0}")]
    UnsupportedOperation(String),

    /// Generic I/O error from sandbox setup.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
