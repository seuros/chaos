// Aggregates all former standalone integration tests as modules.
#[path = "suite/add_dir.rs"]
mod add_dir;
#[path = "suite/apply_patch.rs"]
mod apply_patch;
#[path = "suite/auth_env.rs"]
mod auth_env;
#[path = "suite/ephemeral.rs"]
mod ephemeral;
#[path = "suite/mcp_required_exit.rs"]
mod mcp_required_exit;
#[path = "suite/originator.rs"]
mod originator;
#[path = "suite/output_schema.rs"]
mod output_schema;
#[path = "suite/resume.rs"]
mod resume;
#[path = "suite/sandbox.rs"]
mod sandbox;
#[path = "suite/server_error_exit.rs"]
mod server_error_exit;
