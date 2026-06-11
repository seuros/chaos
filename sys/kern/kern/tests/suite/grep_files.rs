use anyhow::Result;
use core_test_support::responses::mount_function_call_agent_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command as StdCommand;

const MODEL_WITH_TOOL: &str = "test-gpt-5.1-codex";

fn ripgrep_available() -> bool {
    StdCommand::new("rg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

macro_rules! skip_if_ripgrep_missing {
    ($ret:expr $(,)?) => {{
        if !ripgrep_available() {
            eprintln!("rg not available in PATH; skipping test");
            return $ret;
        }
    }};
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grep_files_tool_collects_matches() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_ripgrep_missing!(Ok(()));

    let server = start_mock_server().await;
    let test = build_test_chaos(&server).await?;

    let search_dir = test.cwd.path().join("src");
    std::fs::create_dir_all(&search_dir)?;
    let alpha = search_dir.join("alpha.rs");
    let beta = search_dir.join("beta.rs");
    let gamma = search_dir.join("gamma.txt");
    std::fs::write(&alpha, "alpha needle\n")?;
    std::fs::write(&beta, "beta needle\n")?;
    std::fs::write(&gamma, "needle in text but excluded\n")?;

    let call_id = "grep-files-collect";
    let arguments = serde_json::json!({
        "pattern": "needle",
        "path": search_dir.to_string_lossy(),
        "include": "*.rs",
    })
    .to_string();

    let mocks =
        mount_function_call_agent_response(&server, call_id, &arguments, "grep_files").await;
    test.submit_turn("please find uses of needle").await?;

    let req = mocks.completion.single_request();
    let (content_opt, success_opt) = req
        .function_call_output_content_and_success(call_id)
        .expect("tool output present");
    let content = content_opt.expect("content present");
    let success = success_opt.unwrap_or(true);
    assert!(
        success,
        "expected success for matches, got content={content}"
    );

    let output = parse_grep_output(&content)?;
    assert_eq!(output["match_count"], 2, "content: {content}");
    assert_eq!(output["limit"], 100, "content: {content}");

    let entries = collect_file_names(&output)?;
    assert_eq!(entries.len(), 2, "content: {content}");
    assert!(
        entries.contains("alpha.rs"),
        "missing alpha.rs in {entries:?}"
    );
    assert!(
        entries.contains("beta.rs"),
        "missing beta.rs in {entries:?}"
    );
    assert!(
        !entries.contains("gamma.txt"),
        "txt file should be filtered out: {entries:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grep_files_tool_reports_empty_results() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_ripgrep_missing!(Ok(()));

    let server = start_mock_server().await;
    let test = build_test_chaos(&server).await?;

    let search_dir = test.cwd.path().join("logs");
    std::fs::create_dir_all(&search_dir)?;
    std::fs::write(search_dir.join("output.txt"), "no hits here")?;

    let call_id = "grep-files-empty";
    let arguments = serde_json::json!({
        "pattern": "needle",
        "path": search_dir.to_string_lossy(),
        "limit": 5,
    })
    .to_string();

    let mocks =
        mount_function_call_agent_response(&server, call_id, &arguments, "grep_files").await;
    test.submit_turn("search again").await?;

    let req = mocks.completion.single_request();
    let (content_opt, success_opt) = req
        .function_call_output_content_and_success(call_id)
        .expect("tool output present");
    let content = content_opt.expect("content present");
    if let Some(success) = success_opt {
        assert!(
            success,
            "expected successful empty search content={content}"
        );
    }
    let output = parse_grep_output(&content)?;
    assert_eq!(output["matches"], serde_json::json!([]));
    assert_eq!(output["match_count"], 0);
    assert_eq!(output["limit"], 5);

    Ok(())
}

#[allow(clippy::expect_used)]
async fn build_test_chaos(server: &wiremock::MockServer) -> Result<TestChaos> {
    let mut builder = test_chaos().with_model(MODEL_WITH_TOOL);
    builder.build(server).await
}

fn parse_grep_output(content: &str) -> Result<Value> {
    Ok(serde_json::from_str(content)?)
}

fn collect_file_names(output: &Value) -> Result<HashSet<String>> {
    Ok(output["matches"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing matches array in grep_files output"))?
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            Path::new(line)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect())
}
