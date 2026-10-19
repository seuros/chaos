use super::*;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use std::collections::HashMap;

fn response(id: &str, value: &str) -> RequestUserInputResponse {
    RequestUserInputResponse {
        answers: HashMap::from([(
            id.into(),
            RequestUserInputAnswer {
                answers: vec![value.into()],
            },
        )]),
    }
}

#[test]
fn confirmation_requires_explicit_known_choice() -> anyhow::Result<()> {
    let form = Form::new("Continue?", &ElicitationSchema::new(Default::default()))
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        form.answer(response("confirmation", "Continue"))
            .map_err(anyhow::Error::msg)?
            .action,
        ElicitationAction::Accept
    );
    assert_eq!(
        form.answer(response("confirmation", "Cancel"))
            .map_err(anyhow::Error::msg)?
            .action,
        ElicitationAction::Decline
    );
    assert!(form.answer(response("confirmation", "yes")).is_err());
    Ok(())
}

#[test]
fn form_converts_bool_without_granting_permissions() -> anyhow::Result<()> {
    let form =
        Form::new("Select value", &ElicitationSchema::confirm()).map_err(anyhow::Error::msg)?;
    let result = form
        .answer(response("field_0", "false"))
        .map_err(anyhow::Error::msg)?;
    assert_eq!(result.content, Some(json!({"confirmed":false})));
    assert!(
        form.args.questions[0]
            .question
            .contains("not authorization")
    );
    assert!(form.answer(response("field_0", "other")).is_err());
    Ok(())
}

#[test]
fn unsupported_forms_fail_before_asking() -> anyhow::Result<()> {
    for property in [
        json!({"type":"string"}),
        json!({"type":"integer"}),
        json!({"type":"string","enum":["a","b","c","d"]}),
    ] {
        let schema: ElicitationSchema =
            serde_json::from_value(json!({"type":"object","properties":{"field":property}}))?;
        assert!(Form::new("Question", &schema).is_err());
    }
    Ok(())
}

#[test]
fn enum_answers_preserve_strings_not_ui_indices() -> anyhow::Result<()> {
    let schema: ElicitationSchema = serde_json::from_value(json!({
        "type":"object", "properties":{"choice":{"type":"string","enum":["true","1","other"]}},
        "required":["choice"]
    }))?;
    let form = Form::new("Question", &schema).map_err(anyhow::Error::msg)?;
    assert_eq!(
        form.answer(response("field_0", "1"))
            .map_err(anyhow::Error::msg)?
            .content,
        Some(json!({"choice":"1"}))
    );
    Ok(())
}
