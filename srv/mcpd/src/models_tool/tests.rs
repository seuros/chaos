use super::*;

#[test]
fn refresh_models_requires_an_explicit_provider() {
    assert!(serde_json::from_value::<RefreshModelsParams>(json!({})).is_err());
    assert!(
        serde_json::from_value::<RefreshModelsParams>(
            json!({"provider": "charm", "command": "ignored"})
        )
        .is_err()
    );
    let params: RefreshModelsParams = serde_json::from_value(json!({"provider": "charm"})).unwrap();
    assert_eq!(params.provider, "charm");
    let info = serde_json::to_value(tool_info()).unwrap();
    assert_eq!(info["name"], "refresh_models");
    assert_eq!(info["inputSchema"]["required"], json!(["provider"]));
    assert_eq!(
        info["outputSchema"]["required"],
        json!(["provider", "models"])
    );
}
