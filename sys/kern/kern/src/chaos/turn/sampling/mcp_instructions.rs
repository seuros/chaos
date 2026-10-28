use std::io;

use chaos_mcp_runtime::McpServerInstructions;
use quick_xml::Writer;
use quick_xml::events::BytesCData;
use quick_xml::events::Event;

pub(super) struct McpInstructionsDocument<'a> {
    servers: &'a [McpServerInstructions],
}

impl<'a> McpInstructionsDocument<'a> {
    pub(super) fn new(servers: &'a [McpServerInstructions]) -> Self {
        Self { servers }
    }

    /// Server identity and model-facing capability summaries, with instructions in CDATA.
    pub(super) fn to_xml(&self) -> io::Result<String> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        writer
            .create_element("mcp_server_instructions")
            .write_inner_content(|writer| {
                for server in self.servers {
                    let capabilities = capability_summary(&server.capabilities);
                    writer
                        .create_element("server")
                        .with_attribute(("name", server.server_name.as_str()))
                        .with_attribute(("version", server.server_version.as_str()))
                        .with_attribute(("capabilities", capabilities.as_str()))
                        .write_inner_content(|writer| {
                            writer.create_element("instructions").write_inner_content(
                                |writer| {
                                    for chunk in BytesCData::escaped(&server.instructions) {
                                        writer.write_event(Event::CData(chunk))?;
                                    }
                                    Ok(())
                                },
                            )?;
                            Ok(())
                        })?;
                }
                Ok(())
            })?;
        String::from_utf8(writer.into_inner())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}

fn capability_summary(capabilities: &serde_json::Value) -> String {
    let Some(capabilities) = capabilities.as_object() else {
        return String::new();
    };
    let mut names = Vec::new();
    let mut has_other = false;
    for name in capabilities.keys() {
        match name.as_str() {
            "prompts" | "resources" | "tools" => names.push(name.as_str()),
            _ => has_other = true,
        }
    }
    // Keep the prompt stable even with serde_json/preserve_order.
    names.sort_unstable();
    if has_other {
        names.push("other*");
    }
    names.join(", ")
}
