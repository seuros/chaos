//! Command parsing and safety utilities shared across Codex crates.

mod shell_detect;

pub mod bash;
pub mod command_safety;
pub mod parse_command;

pub use command_safety::is_dangerous_command;
pub use command_safety::is_safe_command;
pub use shell_detect::KnownShell;
pub use shell_detect::detect_shell_type;
