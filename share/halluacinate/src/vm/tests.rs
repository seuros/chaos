use std::fs;

use serde_json::json;

use super::*;

#[tokio::test]
async fn vm_suite() {
    parses_plain_string_statusline_result();
    statusline_renderer_receives_context_and_returns_spans().await;
    default_statusline_renderer_is_available_without_user_scripts().await;
}

fn parses_plain_string_statusline_result() {
    let lua = Lua::new();
    let value = Value::String(lua.create_string("ready").unwrap());

    let spans = parse_statusline_result(value).unwrap();

    assert_eq!(
        spans,
        vec![StatusLineSpan {
            text: "ready".to_string(),
            color: None,
            bold: false,
            line_break: false,
        }]
    );
}

async fn statusline_renderer_receives_context_and_returns_spans() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path().join(".chaos").join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::write(
        scripts_dir.join("status.lua"),
        r#"
chaos.statusline(function(ctx)
  return {
    { text = ctx.model, color = "green", bold = true },
    { text = " " .. ctx.cwd },
  }
end)
"#,
    )
    .unwrap();

    let handle = crate::spawn(SessionInfo {
        session_id: "session".to_string(),
        cwd: temp.path().to_string_lossy().to_string(),
        provider: "test".to_string(),
        user_scripts: Vec::new(),
        project_scripts_enabled: true,
    })
    .unwrap();

    let spans = handle
        .render_statusline(json!({
            "model": "gpt-test",
            "cwd": "/work/repo",
        }))
        .await
        .unwrap();
    handle.shutdown().await;

    assert_eq!(
        spans,
        vec![
            StatusLineSpan {
                text: "gpt-test".to_string(),
                color: Some("green".to_string()),
                bold: true,
                line_break: false,
            },
            StatusLineSpan {
                text: " /work/repo".to_string(),
                color: None,
                bold: false,
                line_break: false,
            },
        ]
    );
}

async fn default_statusline_renderer_is_available_without_user_scripts() {
    let temp = tempfile::tempdir().unwrap();
    let handle = crate::spawn(SessionInfo {
        session_id: "session".to_string(),
        cwd: temp.path().to_string_lossy().to_string(),
        provider: "test".to_string(),
        user_scripts: Vec::new(),
        project_scripts_enabled: true,
    })
    .unwrap();

    let spans = handle
        .render_statusline(json!({
            "model": "gpt-test",
            "reasoning_effort": "high",
            "cwd_display": "~/repo",
            "context": {
                "remaining_pct": 87,
            },
        }))
        .await
        .unwrap();
    handle.shutdown().await;

    let text = spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert_eq!(text, "HUD · HP [===-] 87% · WPN gpt-test high · MAP ~/repo");
}

#[tokio::test]
async fn stored_user_scripts_load_and_untrusted_project_scripts_do_not() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path().join(".chaos").join("scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::write(
        scripts_dir.join("project.lua"),
        r#"chaos.tool({ name = "from_project", description = "p",
  input_schema = { type = "object" }, handler = function() return "p" end })"#,
    )
    .unwrap();
    let stored = crate::ScriptSource {
        name: "stored".to_string(),
        source: r#"chaos.tool({ name = "from_store", description = "s",
  input_schema = { type = "object" }, handler = function() return "s" end })"#
            .to_string(),
    };

    let untrusted = crate::spawn(SessionInfo {
        session_id: "session".to_string(),
        cwd: temp.path().to_string_lossy().to_string(),
        provider: "test".to_string(),
        user_scripts: vec![stored.clone()],
        project_scripts_enabled: false,
    })
    .unwrap();
    let names: Vec<String> = untrusted
        .list_tools()
        .await
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, vec!["from_store".to_string()]);

    let trusted = crate::spawn(SessionInfo {
        session_id: "session".to_string(),
        cwd: temp.path().to_string_lossy().to_string(),
        provider: "test".to_string(),
        user_scripts: vec![stored],
        project_scripts_enabled: true,
    })
    .unwrap();
    let mut names: Vec<String> = trusted
        .list_tools()
        .await
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["from_project".to_string(), "from_store".to_string()]
    );
}
