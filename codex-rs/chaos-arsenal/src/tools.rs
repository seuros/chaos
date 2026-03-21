#[path = "tools/read_file.rs"]
pub mod read_file;

#[path = "tools/grep_files.rs"]
pub mod grep_files;

#[path = "tools/list_dir.rs"]
pub mod list_dir;

use mcp_host::registry::router::McpToolRouter;

use crate::ChaosServer;

pub fn router() -> McpToolRouter<ChaosServer> {
    let router = McpToolRouter::new();
    let router = read_file::mount(router);
    let router = grep_files::mount(router);
    list_dir::mount(router)
}
