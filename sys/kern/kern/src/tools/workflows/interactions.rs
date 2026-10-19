//! Native form interaction through ordinary, dynamically authorized dispatch.
//! Sampling uses the session's isolated, bounded text-only provider path.

use super::dispatch::Caller;
use chaos_ipc::request_user_input::{
    RequestUserInputArgs, RequestUserInputQuestion, RequestUserInputQuestionOption,
    RequestUserInputResponse,
};
use mcp_host::bt::{InteractionFuture, TreeInteractionContext, TreeInteractions};
use mcp_host::protocol::elicitation::ElicitationSchema;
use mcp_host::protocol::types::{
    CreateElicitationResult, CreateMessageParams, CreateMessageResult, ElicitationAction,
};
use serde_json::{Map, Value, json};

pub(super) struct NativeInteractions {
    pub(super) caller: Caller,
}

impl TreeInteractions for NativeInteractions {
    fn sample<'a>(
        &'a self,
        ctx: TreeInteractionContext<'a>,
        params: CreateMessageParams,
    ) -> InteractionFuture<'a, CreateMessageResult> {
        Box::pin(async move {
            let id = ctx.step.id.as_deref().ok_or("missing host step identity")?;
            tracing::debug!(
                run_id = %self.caller.run_id, node_id = id, attempt = ctx.attempt,
                "native workflow sample started"
            );
            super::sampling::sample(&self.caller, params).await
        })
    }

    fn ask<'a>(
        &'a self,
        ctx: TreeInteractionContext<'a>,
        message: String,
        schema: &'a ElicitationSchema,
    ) -> InteractionFuture<'a, CreateElicitationResult> {
        Box::pin(async move {
            // Reject unrepresentable forms before requesting any input.
            let form = Form::new(&message, schema)?;
            let id = ctx.step.id.as_deref().ok_or("missing host step identity")?;
            let output = self
                .caller
                .call(
                    "request_user_input",
                    id,
                    ctx.attempt,
                    false,
                    serde_json::to_value(&form.args).map_err(|error| error.to_string())?,
                )
                .await
                .map_err(|error| error.to_string())?
                .into_payload();
            let response: RequestUserInputResponse =
                serde_json::from_value(output).map_err(|error| error.to_string())?;
            form.answer(response)
        })
    }
}

struct Field {
    name: String,
    choices: Vec<(String, Value)>,
}

struct Form {
    args: RequestUserInputArgs,
    fields: Vec<Field>,
}

impl Form {
    fn new(message: &str, schema: &ElicitationSchema) -> Result<Self, String> {
        let encoded = serde_json::to_value(schema).map_err(|error| error.to_string())?;
        let mut fields = Vec::new();
        for (name, property) in encoded["properties"]
            .as_object()
            .ok_or("invalid form properties")?
        {
            // The existing native picker has no typed numeric/text/array
            // widget. Do not fabricate answers or silently drop constraints.
            let values = if property["type"] == "boolean" {
                vec![Value::Bool(true), Value::Bool(false)]
            } else if let Some(values) = property["enum"].as_array() {
                values.clone()
            } else if let Some(options) = property["oneOf"].as_array() {
                options
                    .iter()
                    .map(|option| {
                        option
                            .get("const")
                            .cloned()
                            .ok_or("unsupported form option")
                    })
                    .collect::<Result<_, _>>()?
            } else {
                return Err(
                    "native Ask supports only boolean and single-select enum fields".into(),
                );
            };
            if !(1..=3).contains(&values.len()) {
                return Err("native Ask needs one to three options per field".into());
            }
            fields.push(Field {
                name: name.clone(),
                choices: values
                    .into_iter()
                    .map(|value| {
                        let label = value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string());
                        (label, value)
                    })
                    .collect(),
            });
        }
        if fields.len() > 3 {
            return Err("native Ask supports at most three fields".into());
        }
        let questions = if fields.is_empty() {
            vec![question(
                "confirmation".into(),
                "Confirm",
                message,
                ["Continue", "Cancel"].into_iter(),
            )]
        } else {
            fields
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    question(
                        format!("field_{index}"),
                        &field.name,
                        &format!("{message}\n{}", field.name),
                        field.choices.iter().map(|(label, _)| label.as_str()),
                    )
                })
                .collect()
        };
        Ok(Self {
            args: RequestUserInputArgs { questions },
            fields,
        })
    }

    fn answer(
        &self,
        response: RequestUserInputResponse,
    ) -> Result<CreateElicitationResult, String> {
        let answer = |id: &str| -> Result<&str, String> {
            let values = &response
                .answers
                .get(id)
                .ok_or("missing user answer")?
                .answers;
            if values.len() != 1 {
                return Err("native Ask requires exactly one answer per field".into());
            }
            Ok(values[0].as_str())
        };
        if self.fields.is_empty() {
            return match answer("confirmation")? {
                "Continue" => Ok(CreateElicitationResult {
                    action: ElicitationAction::Accept,
                    content: Some(json!({})),
                }),
                "Cancel" => Ok(CreateElicitationResult {
                    action: ElicitationAction::Decline,
                    content: None,
                }),
                _ => Err("unrecognized confirmation answer".into()),
            };
        }
        let mut content = Map::new();
        for (index, field) in self.fields.iter().enumerate() {
            let answer = answer(&format!("field_{index}"))?;
            let value = field
                .choices
                .iter()
                .find(|(label, _)| label == answer)
                .map(|(_, value)| value.clone())
                .ok_or("answer is not a declared form option")?;
            content.insert(field.name.clone(), value);
        }
        Ok(CreateElicitationResult {
            action: ElicitationAction::Accept,
            content: Some(Value::Object(content)),
        })
    }
}

fn question<'a>(
    id: String,
    header: &str,
    message: &str,
    choices: impl Iterator<Item = &'a str>,
) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
        id,
        header: header.chars().take(12).collect(),
        question: format!("{message}\nThis is input only, not authorization for other actions."),
        is_other: false,
        is_secret: false,
        options: Some(
            choices
                .map(|label| RequestUserInputQuestionOption {
                    label: label.into(),
                    description: "Use this value for the workflow input.".into(),
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests;
