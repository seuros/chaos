use core_test_support::responses::mount_function_call_agent_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::test_chaos;
use pretty_assertions::assert_eq;
use serde_json::json;

fn assert_list_dir_output(
    output: &str,
    absolute_path: &str,
    depth: u64,
    offset: u64,
    limit: u64,
    entries: &[&str],
) -> anyhow::Result<()> {
    let output: serde_json::Value = serde_json::from_str(output)?;
    assert_eq!(output["absolute_path"], json!(absolute_path));
    assert_eq!(output["depth"], json!(depth));
    assert_eq!(output["offset"], json!(offset));
    assert_eq!(output["limit"], json!(limit));
    assert_eq!(output["entries"], json!(entries));
    assert_eq!(output["truncated"], json!(false));
    assert_eq!(output["truncation_message"], serde_json::Value::Null);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_tool_returns_entries() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_chaos().build(&server).await?;

    let dir_path = test.cwd.path().join("sample_dir");
    std::fs::create_dir(&dir_path)?;
    std::fs::write(dir_path.join("alpha.txt"), "first file")?;
    std::fs::create_dir(dir_path.join("nested"))?;
    let dir_path = dir_path.to_string_lossy().to_string();

    let call_id = "list-dir-call";
    let arguments = json!({
        "dir_path": dir_path.clone(),
        "offset": 1,
        "limit": 2,
    })
    .to_string();

    let mocks = mount_function_call_agent_response(&server, call_id, &arguments, "list_dir").await;
    test.submit_turn("list directory contents").await?;
    let req = mocks.completion.single_request();
    let (content_opt, _) = req
        .function_call_output_content_and_success(call_id)
        .expect("function_call_output present");
    let output = content_opt.expect("output content present in tool output");
    assert_list_dir_output(&output, &dir_path, 2, 1, 2, &["alpha.txt", "nested/"])?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_tool_depth_one_omits_children() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_chaos().build(&server).await?;

    let dir_path = test.cwd.path().join("depth_one");
    std::fs::create_dir(&dir_path)?;
    std::fs::write(dir_path.join("alpha.txt"), "alpha")?;
    std::fs::create_dir(dir_path.join("nested"))?;
    std::fs::write(dir_path.join("nested").join("beta.txt"), "beta")?;
    let dir_path = dir_path.to_string_lossy().to_string();

    let call_id = "list-dir-depth1";
    let arguments = json!({
        "dir_path": dir_path.clone(),
        "offset": 1,
        "limit": 10,
        "depth": 1,
    })
    .to_string();

    let mocks = mount_function_call_agent_response(&server, call_id, &arguments, "list_dir").await;
    test.submit_turn("list directory contents depth one")
        .await?;
    let req = mocks.completion.single_request();
    let (content_opt, _) = req
        .function_call_output_content_and_success(call_id)
        .expect("function_call_output present");
    let output = content_opt.expect("output content present in tool output");
    assert_list_dir_output(&output, &dir_path, 1, 1, 10, &["alpha.txt", "nested/"])?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_tool_depth_two_includes_children_only() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_chaos().build(&server).await?;

    let dir_path = test.cwd.path().join("depth_two");
    std::fs::create_dir(&dir_path)?;
    std::fs::write(dir_path.join("alpha.txt"), "alpha")?;
    let nested = dir_path.join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(nested.join("beta.txt"), "beta")?;
    let deeper = nested.join("grand");
    std::fs::create_dir(&deeper)?;
    std::fs::write(deeper.join("gamma.txt"), "gamma")?;
    let dir_path_string = dir_path.to_string_lossy().to_string();

    let call_id = "list-dir-depth2";
    let arguments = json!({
        "dir_path": dir_path_string.clone(),
        "offset": 1,
        "limit": 10,
        "depth": 2,
    })
    .to_string();

    let mocks = mount_function_call_agent_response(&server, call_id, &arguments, "list_dir").await;
    test.submit_turn("list directory contents depth two")
        .await?;
    let req = mocks.completion.single_request();
    let (content_opt, _) = req
        .function_call_output_content_and_success(call_id)
        .expect("function_call_output present");
    let output = content_opt.expect("output content present in tool output");
    assert_list_dir_output(
        &output,
        &dir_path_string,
        2,
        1,
        10,
        &["alpha.txt", "nested/", "  beta.txt", "  grand/"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_tool_depth_three_includes_grandchildren() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_chaos().build(&server).await?;

    let dir_path = test.cwd.path().join("depth_three");
    std::fs::create_dir(&dir_path)?;
    std::fs::write(dir_path.join("alpha.txt"), "alpha")?;
    let nested = dir_path.join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(nested.join("beta.txt"), "beta")?;
    let deeper = nested.join("grand");
    std::fs::create_dir(&deeper)?;
    std::fs::write(deeper.join("gamma.txt"), "gamma")?;
    let dir_path_string = dir_path.to_string_lossy().to_string();

    let call_id = "list-dir-depth3";
    let arguments = json!({
        "dir_path": dir_path_string.clone(),
        "offset": 1,
        "limit": 10,
        "depth": 3,
    })
    .to_string();

    let mocks = mount_function_call_agent_response(&server, call_id, &arguments, "list_dir").await;
    test.submit_turn("list directory contents depth three")
        .await?;
    let req = mocks.completion.single_request();
    let (content_opt, _) = req
        .function_call_output_content_and_success(call_id)
        .expect("function_call_output present");
    let output = content_opt.expect("output content present in tool output");
    assert_list_dir_output(
        &output,
        &dir_path_string,
        3,
        1,
        10,
        &[
            "alpha.txt",
            "nested/",
            "  beta.txt",
            "  grand/",
            "    gamma.txt",
        ],
    )?;

    Ok(())
}
