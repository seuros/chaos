use super::*;

#[test]
fn appends_labeled_mcp_server_instructions() {
    let mut base = chaos_ipc::models::BaseInstructions {
        text: "Base instructions.".to_string(),
    };

    append_mcp_server_instructions(
        &mut base,
        &[
            McpServerInstructions {
                server_name: "alpha & \"tools\"".to_string(),
                instructions: "Use Vec<T> & preserve\n  ]]> literally.".to_string(),
            },
            McpServerInstructions {
                server_name: "beta".to_string(),
                instructions: "Prefer beta resources.".to_string(),
            },
        ],
    )
    .unwrap();

    assert!(base.text.starts_with("Base instructions."));
    let xml = base.text.split_once("<mcp_server_instructions>").unwrap().1;
    assert_eq!(
        xml,
        r#"
  <server name="alpha &amp; &quot;tools&quot;">
    <instructions><![CDATA[Use Vec<T> & preserve
  ]]]]><![CDATA[> literally.]]></instructions>
  </server>
  <server name="beta">
    <instructions><![CDATA[Prefer beta resources.]]></instructions>
  </server>
</mcp_server_instructions>"#
    );
}

#[test]
fn leaves_base_instructions_unchanged_without_mcp_instructions() {
    let mut base = chaos_ipc::models::BaseInstructions {
        text: "Base instructions.".to_string(),
    };

    append_mcp_server_instructions(&mut base, &[]).unwrap();

    assert_eq!(base.text, "Base instructions.");
}
