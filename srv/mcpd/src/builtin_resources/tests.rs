use super::*;

#[test]
fn only_chaos_mcp_builtin_resource_is_subscribable() -> Result<(), serde_json::Error> {
    for spec in builtin_mcp_resources::resource_specs() {
        let info = resource_info(spec);
        assert_eq!(info.subscribable, info.uri == "chaos://mcp", "{info:?}");
        let wire = serde_json::to_value(info)?;
        assert!(wire.get("subscribable").is_none());
    }
    Ok(())
}

#[test]
fn builtin_resource_templates_are_not_subscribable() -> Result<(), serde_json::Error> {
    for spec in builtin_mcp_resources::resource_template_specs() {
        let info = template_info(spec);
        assert!(!info.subscribable, "{info:?}");
        let wire = serde_json::to_value(info)?;
        assert!(wire.get("subscribable").is_none());
    }
    Ok(())
}
