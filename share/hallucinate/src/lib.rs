//! Chaos Hallucinate — multi-engine scripting runtime.
//!
//! Embeds sandboxed script engines and exposes Chaos internals to user
//! scripts. Custom tools, hooks, policy rules, workflow automation — all
//! without recompiling.
//!
//! Supported backends:
//! - **Lua 5.4** (always available) — `.lua` scripts
//! - **WASM** (behind `wasm` feature) — `.wasm` modules
//!
//! Scripts are discovered from `~/.config/chaos/scripts/` (user layer)
//! and `.chaos/scripts/` (project layer), loaded in lexicographic order.

pub mod api;
pub mod discovery;
pub mod engine;
pub mod handle;
pub mod sandbox;
pub mod vm;

pub use api::SessionInfo;
pub use engine::ScriptEngine;
pub use handle::HallucinateHandle;
pub use handle::HookResult;
pub use handle::ScriptTool;
pub use handle::ToolResult;
pub use vm::spawn;
