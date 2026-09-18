use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn test_config_disables_host_probes_unless_explicitly_enabled() -> Result<()> {
    let home = TempDir::new()?;
    let base_url = "http://127.0.0.1:1/v1";
    let (config, _cwd) = test_chaos().prepare_config(base_url.into(), &home).await?;
    assert!(!config.machine_warnings.enabled);

    let (config, _cwd) = test_chaos()
        .with_config(|config| config.machine_warnings.enabled = true)
        .prepare_config(base_url.into(), &home)
        .await?;
    assert!(config.machine_warnings.enabled);
    Ok(())
}

#[test]
fn custom_tool_call_output_text_returns_output_text() {
    let bodies = vec![json!({
        "input": [{
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": "hello"
        }]
    })];

    assert_eq!(custom_tool_call_output_text(&bodies, "call-1"), "hello");
}

#[test]
#[should_panic(expected = "custom_tool_call_output call-2 missing output")]
fn custom_tool_call_output_text_panics_when_output_is_missing() {
    let bodies = vec![json!({
        "input": [{
            "type": "custom_tool_call_output",
            "call_id": "call-2"
        }]
    })];

    let _ = custom_tool_call_output_text(&bodies, "call-2");
}
