use super::*;

#[tokio::test]
async fn editor_and_disk_sources_share_exact_slice_and_indentation_results() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("format.rs");
    for text in [
        "fn main() {\r\n\t// header\r\n\tbody();\r\n}\r\n",
        "one\n\nthree",
        "🦀\n",
    ] {
        tokio::fs::write(&file, text).await.unwrap();
        for mode in ["slice", "indentation"] {
            let args = serde_json::json!({
                "file_path":file,"offset":1,"limit":3,"mode":mode
            });
            assert_eq!(
                execute_structured(&args).await,
                execute_editor_structured(&args, text).await,
            );
        }
    }
}

#[tokio::test]
async fn indentation_capacity_is_bounded_by_available_lines_not_requested_limit() {
    let args = serde_json::json!({
        "file_path":"/existing-buffer.rs",
        "mode":"indentation",
        "limit":usize::MAX,
    });
    let output = execute_editor_structured(&args, "first\nsecond\n")
        .await
        .unwrap();
    assert_eq!(output["line_count"], 2);
}
