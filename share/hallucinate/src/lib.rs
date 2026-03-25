//! Chaos Hallucinate — HAL+LUA scripting engine.
//!
//! Embeds a sandboxed Lua 5.4 runtime and exposes Chaos internals to
//! user scripts. Custom tools, hooks, policy rules, workflow automation
//! — all without recompiling.
//!
//! Scripts are discovered from `~/.config/chaos/scripts/` (user layer)
//! and `.chaos/scripts/` (project layer), loaded in lexicographic order.

pub mod api;
pub mod discovery;
pub mod handle;
pub mod sandbox;
pub mod vm;

pub use api::SessionInfo;
pub use handle::HallucinateHandle;
pub use handle::HookResult;
pub use handle::LuaTool;
pub use handle::ToolResult;
pub use vm::spawn;
