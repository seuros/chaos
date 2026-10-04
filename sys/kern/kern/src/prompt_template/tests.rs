use super::*;
use mcp_host::prelude::ToolGroupCatalog;

fn tools(names: &str) -> Vec<ToolSpec> {
    names
        .split_whitespace()
        .map(|name| {
            ToolSpec::Function(chaos_parrot::sanitize::ResponsesApiTool {
                name: name.to_owned(),
                description: String::new(),
                strict: false,
                defer_loading: None,
                parameters: chaos_parrot::sanitize::JsonSchema::Object {
                    properties: Default::default(),
                    required: None,
                    additional_properties: None,
                },
                output_schema: None,
            })
        })
        .collect()
}

#[test]
fn renderer_uses_plain_text_strict_fields_and_product_global() -> anyhow::Result<()> {
    let engine = template_environment();
    let value = "<model>& {{ literal }}";
    assert_eq!(
        engine.render_str("{{ OS_NAME }}|{{ value }}\n", minijinja::context!(value))?,
        format!("{OS_NAME}|{value}\n")
    );
    assert_eq!(
        engine
            .render_str("{{ missing }}", minijinja::context!())
            .unwrap_err()
            .kind(),
        ErrorKind::UndefinedError
    );
    Ok(())
}

#[test]
fn bundled_runtime_renders() {
    let catalog = crate::tools::groups::build_catalog().unwrap();
    let state = catalog.new_state();
    let text = runtime_instructions(
        &tools("enable_tools read_session_history call_mcp_tool_async write_stdin"),
        ToolGroupFilter {
            catalog: &catalog,
            state: &state,
        },
        ModeCapabilities::default(),
        &SessionSource::Cli,
        false,
        true,
    );
    assert!(!text.is_empty());
}

#[test]
fn progress_guidance_requires_an_attached_plan_tool() {
    let catalog = crate::tools::groups::build_catalog().unwrap();
    let state = catalog.new_state();
    let render = |names| {
        runtime_instructions(
            &tools(names),
            ToolGroupFilter {
                catalog: &catalog,
                state: &state,
            },
            ModeCapabilities::default(),
            &SessionSource::Api,
            false,
            false,
        )
    };
    let unattached = render("switch_mode read_file");
    assert!(!unattached.contains("# Attached plan"));
    assert!(!unattached.contains("plan_progress"));
    let attached = render("switch_mode plan_progress");
    assert!(attached.contains("# Attached plan"));
    assert!(attached.contains("switch to Plan mode yourself"));
    let fixed = render("plan_progress");
    assert!(fixed.contains("# Attached plan"));
    assert!(!fixed.contains("switch to Plan mode yourself"));
}

#[test]
fn runtime_selects_sections_from_capabilities() -> anyhow::Result<()> {
    let mut engine = template_environment();
    for section in ["tools", "resources", "mcp", "jobs", "editing", "terminal"] {
        engine.add_template_owned(format!("_{section}.md.j2"), format!("section:{section}\n"))?;
    }
    let template = engine.get_template(RUNTIME_TEMPLATE_NAME)?;
    let catalog = crate::tools::groups::build_catalog().unwrap();

    for (names, enabled_groups, mutation, terminal, expected) in [
        ("", "", true, false, ""),
        ("read_file", "", true, false, "tools resources"),
        (
            "enable_tools",
            "",
            true,
            false,
            "tools resources mcp editing",
        ),
        (
            "enable_tools",
            "mcp-management editing",
            true,
            false,
            "tools resources",
        ),
        (
            "enable_tools mcp_add_server apply_patch write_stdin",
            "mcp-management editing shell",
            true,
            false,
            "tools resources mcp editing jobs",
        ),
        ("enable_tools", "", false, false, "tools resources"),
        ("", "", true, true, "terminal"),
    ] {
        let state = catalog.new_state();
        catalog.set_groups_enabled(&state, enabled_groups.split_whitespace(), true)?;
        let specs = tools(names);
        let mut context = RuntimeContext::new(
            &specs,
            ToolGroupFilter {
                catalog: &catalog,
                state: &state,
            },
            ModeCapabilities {
                mutation,
                ..Default::default()
            },
            &SessionSource::Api,
            false,
            false,
        );
        context.output.terminal = terminal;
        let rendered = template.render(context)?;
        let sections = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("section:"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            sections,
            expected.split_whitespace().collect(),
            "tools={names}, enabled_groups={enabled_groups}, mutation={mutation}, terminal={terminal}"
        );
    }
    Ok(())
}

#[test]
fn terminal_context_requires_tui_and_interactive_console_output() {
    let catalog = ToolGroupCatalog::new();
    let state = catalog.new_state();
    for (source, tui_output, structured, expected) in [
        (SessionSource::Cli, true, false, cfg!(feature = "tui")),
        (SessionSource::Cli, false, false, false),
        (SessionSource::Cli, true, true, false),
        (SessionSource::Api, true, false, false),
        (SessionSource::Exec, true, false, false),
        (
            SessionSource::SubAgent(chaos_ipc::protocol::SubAgentSource::Review),
            true,
            false,
            false,
        ),
    ] {
        let context = RuntimeContext::new(
            &[],
            ToolGroupFilter {
                catalog: &catalog,
                state: &state,
            },
            ModeCapabilities::default(),
            &source,
            structured,
            tui_output,
        );
        assert_eq!(
            context.output.terminal, expected,
            "{source:?}, structured={structured}, tui_output={tui_output}"
        );
    }
}
