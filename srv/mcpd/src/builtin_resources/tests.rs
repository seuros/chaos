use super::*;

#[test]
fn builtin_resources_are_not_subscribable() -> Result<(), serde_json::Error> {
    for spec in builtin_mcp_resources::resource_specs() {
        let info = resource_info(spec);
        assert!(!info.subscribable, "{info:?}");
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
