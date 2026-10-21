use acp_host::{SessionContext, protocol};
use chaos_ipc::approvals::{ElicitationRequest, ElicitationRequestEvent};
use chaos_ipc::protocol::ElicitationAction as NativeAction;
use chaos_ipc::request_user_input::{
    RequestUserInputAnswer, RequestUserInputEvent, RequestUserInputResponse,
};
use protocol::schema::v1::{
    CreateElicitationRequest, ElicitationAction, ElicitationFormMode, ElicitationMode,
    ElicitationSessionScope, ElicitationUrlMode,
};
use serde_json::{Value, json};

pub(crate) fn url_id(context: &SessionContext, server: &str, id: &str) -> String {
    // ACP IDs are connection-wide; MCP IDs are only server-local.
    json!([context.session_id().0.as_ref(), server, id]).to_string()
}

pub(crate) async fn request(
    context: &SessionContext,
    event: &ElicitationRequestEvent,
) -> (NativeAction, Option<Value>) {
    let scope = ElicitationSessionScope::new(context.session_id().clone());
    let mode: ElicitationMode = match &event.request {
        ElicitationRequest::Form {
            requested_schema, ..
        } => {
            let Ok(schema) = serde_json::from_value(requested_schema.clone()) else {
                return (NativeAction::Cancel, None);
            };
            ElicitationFormMode::new(scope, schema).into()
        }
        ElicitationRequest::Url {
            url,
            elicitation_id,
            ..
        } => ElicitationUrlMode::new(
            scope,
            url_id(context, &event.server_name, elicitation_id),
            url.clone(),
        )
        .into(),
    };
    let request = CreateElicitationRequest::new(mode, event.request.message());
    let Ok(response) = context.client().create_elicitation(request).await else {
        return (NativeAction::Cancel, None);
    };
    match response.action {
        ElicitationAction::Accept(accept) => {
            let content = accept
                .content
                .and_then(|content| serde_json::to_value(content).ok());
            (NativeAction::Accept, content)
        }
        ElicitationAction::Decline => (NativeAction::Decline, None),
        _ => (NativeAction::Cancel, None),
    }
}

pub(crate) async fn user_input(
    context: &SessionContext,
    event: &RequestUserInputEvent,
) -> RequestUserInputResponse {
    let mut response = RequestUserInputResponse {
        answers: Default::default(),
    };
    // ACP forms are not a secret collection channel.
    if event.questions.iter().any(|question| question.is_secret) {
        return response;
    }
    let properties: serde_json::Map<String, Value> = event
        .questions
        .iter()
        .map(|question| {
            let choices = question
                .options
                .as_ref()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| format!("{}: {}", option.label, option.description))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let description = if choices.is_empty() {
                question.question.clone()
            } else {
                format!("{}\n{}", question.question, choices)
            };
            let mut field =
                json!({"type":"string","title":question.header,"description":description});
            if !question.is_other
                && let Some(options) = &question.options
            {
                field["enum"] = json!(
                    options
                        .iter()
                        .map(|option| &option.label)
                        .collect::<Vec<_>>()
                );
            }
            (question.id.clone(), field)
        })
        .collect();
    let schema = json!({"type":"object","properties":properties,
        "required":event.questions.iter().map(|question| &question.id).collect::<Vec<_>>()});
    let Ok(schema) = serde_json::from_value(schema) else {
        return response;
    };
    let request = CreateElicitationRequest::new(
        ElicitationFormMode::new(
            ElicitationSessionScope::new(context.session_id().clone()),
            schema,
        ),
        "Input requested by the agent",
    );
    let Ok(reply) = context.client().create_elicitation(request).await else {
        return response;
    };
    if let ElicitationAction::Accept(accepted) = reply.action
        && let Some(content) = accepted.content
    {
        let Ok(Value::Object(content)) = serde_json::to_value(content) else {
            return response;
        };
        for question in &event.questions {
            let Some(answer) = content.get(&question.id).and_then(Value::as_str) else {
                return RequestUserInputResponse {
                    answers: Default::default(),
                };
            };
            if !question.is_other
                && question
                    .options
                    .as_ref()
                    .is_some_and(|options| !options.iter().any(|option| option.label == answer))
            {
                return RequestUserInputResponse {
                    answers: Default::default(),
                };
            }
            response.answers.insert(
                question.id.clone(),
                RequestUserInputAnswer {
                    answers: vec![answer.into()],
                },
            );
        }
    }
    response
}
