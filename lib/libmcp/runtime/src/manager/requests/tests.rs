use super::*;

#[test]
fn tool_arguments_preserve_objects_and_contextual_errors() {
    for operation in ["tool call", "async tool call"] {
        assert_eq!(
            tool_arguments(None, operation).expect("absent arguments"),
            None
        );
        let object = serde_json::json!({"message": "hello"});
        assert_eq!(
            tool_arguments(Some(object.clone()), operation).expect("object arguments"),
            object.as_object().cloned()
        );
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!("not an object"),
            serde_json::json!(7),
        ] {
            let expected = format!(
                "{operation} arguments must be a JSON object, got {}",
                invalid.to_string().chars().take(80).collect::<String>()
            );
            assert_eq!(
                tool_arguments(Some(invalid), operation)
                    .expect_err("invalid arguments")
                    .to_string(),
                expected
            );
        }
    }
}

#[tokio::test]
async fn collecting_server_results_keeps_successes_after_errors_and_cancelled_tasks() {
    let mut calls = JoinSet::<(String, Result<u8>)>::new();
    calls.spawn(async { ("ready".to_string(), Ok(7)) });
    calls.spawn(async { ("failed".to_string(), Err(anyhow!("server failed"))) });
    calls.spawn(futures::future::pending()).abort();
    assert_eq!(
        collect_server_results(calls, "test list").await,
        HashMap::from([("ready".to_string(), 7)])
    );
}
