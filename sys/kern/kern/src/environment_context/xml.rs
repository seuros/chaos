use std::io;

use quick_xml::Writer;
use quick_xml::events::BytesCData;
use quick_xml::events::BytesText;
use quick_xml::events::Event;

use super::EnvironmentContext;
use super::platform::PlatformContext;

impl EnvironmentContext {
    #[allow(clippy::expect_used)]
    pub fn serialize_to_xml(&self) -> String {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        self.write_xml(&mut writer)
            .expect("writing XML to a Vec cannot fail");
        String::from_utf8(writer.into_inner()).expect("XML is written from UTF-8 strings")
    }

    fn write_xml(&self, writer: &mut Writer<Vec<u8>>) -> io::Result<()> {
        writer
            .create_element("environment_context")
            .write_inner_content(|writer| {
                if let Some(platform) = &self.platform {
                    platform.write_xml(writer)?;
                }
                writer
                    .create_element("shell")
                    .with_attribute(("name", self.shell.name()))
                    .with_attribute(("path", self.shell.shell_path.to_string_lossy().as_ref()))
                    .write_empty()?;
                if let Some(cwd) = &self.cwd {
                    text(writer, "cwd", &cwd.to_string_lossy())?;
                }
                if let Some(date) = &self.current_date {
                    text(writer, "current_date", date)?;
                }
                if let Some(timezone) = &self.timezone {
                    text(writer, "timezone", timezone)?;
                }
                if let Some(network) = &self.network {
                    writer
                        .create_element("network")
                        .with_attribute(("enabled", "true"))
                        .write_inner_content(|writer| {
                            for domain in &network.allowed_domains {
                                text(writer, "allowed", domain)?;
                            }
                            for domain in &network.denied_domains {
                                text(writer, "denied", domain)?;
                            }
                            Ok(())
                        })?;
                }
                if let Some(subagents) = &self.subagents {
                    writer
                        .create_element("subagents")
                        .write_inner_content(|writer| {
                            for chunk in BytesCData::escaped(subagents) {
                                writer.write_event(Event::CData(chunk))?;
                            }
                            Ok(())
                        })?;
                }
                Ok(())
            })?;
        Ok(())
    }
}

impl PlatformContext {
    fn write_xml(&self, writer: &mut Writer<Vec<u8>>) -> io::Result<()> {
        writer
            .create_element("platform")
            .write_inner_content(|writer| {
                text(writer, "os", &self.os)?;
                if let Some(version) = &self.os_version {
                    text(writer, "os_version", version)?;
                }
                text(writer, "kernel", &self.kernel)?;
                if let Some(release) = &self.kernel_release {
                    text(writer, "kernel_release", release)?;
                }
                text(writer, "arch", &self.arch)?;
                if let Some(distribution) = &self.distribution {
                    let mut element = writer
                        .create_element("distribution")
                        .with_attribute(("name", distribution.name.as_str()));
                    if let Some(version) = &distribution.version {
                        element = element.with_attribute(("version", version.as_str()));
                    }
                    element.write_empty()?;
                }
                Ok(())
            })?;
        Ok(())
    }
}

fn text(writer: &mut Writer<Vec<u8>>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element(name)
        .write_text_content(BytesText::new(value))?;
    Ok(())
}
