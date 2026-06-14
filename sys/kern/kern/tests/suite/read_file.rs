use core_test_support::responses::mount_function_call_agent_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::test_chaos;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_file_tool_returns_requested_lines() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_chaos().build(&server).await?;

    let file_path = test.cwd.path().join("sample.txt");
    std::fs::write(&file_path, "first\nsecond\nthird\nfourth\n")?;
    let file_path = file_path.to_string_lossy().to_string();

    let call_id = "read-file-call";
    let arguments = json!({
        "file_path": file_path.clone(),
        "offset": 2,
        "limit": 2,
    })
    .to_string();

    let mocks = mount_function_call_agent_response(&server, call_id, &arguments, "read_file").await;

    test.submit_turn("please inspect sample.txt").await?;

    let req = mocks.completion.single_request();
    let (output_text_opt, _) = req
        .function_call_output_content_and_success(call_id)
        .expect("output present");
    let output_text = output_text_opt.expect("output text present");
    let output: serde_json::Value = serde_json::from_str(&output_text)?;
    assert_eq!(output["file_path"], json!(file_path));
    assert_eq!(output["mode"], json!("slice"));
    assert_eq!(output["offset"], json!(2));
    assert_eq!(output["limit"], json!(2));
    assert_eq!(output["line_count"], json!(2));
    assert_eq!(output["lines"], json!(["L2: second", "L3: third"]));
    assert_eq!(output["text"], json!("L2: second\nL3: third"));

    Ok(())
}
