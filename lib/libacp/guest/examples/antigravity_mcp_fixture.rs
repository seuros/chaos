//! Controlled MCP fixture, NOT the Chaos kernel/session bridge.
//! Reads only a test-selected private file, with no model-selected paths.
use std::io::Write;
use std::path::PathBuf;

use mcp_host::prelude::{Server, StdioTransport};
use mcp_host::registry::tools::{Tool, ToolError, ToolFuture, ToolOutput};
use mcp_host::server::visibility::ExecutionContext;
use serde_json::{Value, json};

struct Fixture {
    file: PathBuf,
    audit: PathBuf,
}

impl Tool for Fixture {
    fn name(&self) -> &str {
        "read_fixture"
    }

    fn description(&self) -> Option<&str> {
        Some("Read the unpredictable test marker from the controlled MCP fixture.")
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    }

    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let mut audit = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.audit)
                .map_err(|_| ToolError::Execution("audit unavailable".into()))?;
            let keys: Vec<_> = ctx
                .params
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys())
                .filter(|key| {
                    key.len() <= 64
                        && key
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
                .take(16)
                .collect();
            writeln!(
                audit,
                "{}",
                json!({
                    "event": "execute_entered",
                    "argument_keys": keys,
                    "null": ctx.params.is_null()
                })
            )
            .map_err(|_| ToolError::Execution("audit write failed".into()))?;
            if ctx
                .params
                .as_object()
                .is_none_or(|object| !object.is_empty())
            {
                return Err(ToolError::Execution("fixture accepts no arguments".into()));
            }
            let marker = std::fs::read_to_string(&self.file)
                .map_err(|_| ToolError::Execution("fixture unavailable".into()))?;
            writeln!(audit, "read_fixture")
                .map_err(|_| ToolError::Execution("audit write failed".into()))?;
            Ok(ToolOutput::text(marker))
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let file = PathBuf::from(args.next().ok_or("missing fixture path")?);
    let audit = PathBuf::from(args.next().ok_or("missing audit path")?);
    if args.next().is_some() || !file.is_absolute() || !audit.is_absolute() {
        return Err("supply absolute fixture and audit paths only".into());
    }
    let mut audit_file = std::fs::OpenOptions::new().append(true).open(&audit)?;
    writeln!(audit_file, "fixture_started")?;
    let server = Server::builder("chaos-acp-test-fixture", "0.1.0")
        .with_tools(true)
        .build();
    server.tool_registry().register(Fixture { file, audit })?;
    server.run(StdioTransport::new()).await?;
    Ok(())
}
