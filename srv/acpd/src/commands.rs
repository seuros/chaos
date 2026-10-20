//! ACP commands dispatch to native operations, never back through a terminal UI
//! or an LLM interpretation of a slash command.
use acp_host::{SessionContext, protocol};
use chaos_ipc::protocol::{Op, ReviewRequest, ReviewTarget};
use chaos_kern::Process;
use chaos_kern::custom_prompts::args::{expand_custom_prompt, parse_slash_name};
use protocol::schema::v1::{
    AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate, ContentBlock, ContentChunk,
    SessionConfigOptionValue, SessionUpdate, SetSessionConfigOptionRequest, TextContent,
    UnstructuredCommandInput,
};

pub(crate) enum Dispatch {
    Prompt(Vec<ContentBlock>),
    Native(Box<Op>),
    Done,
}

fn command(name: &str, description: &str, hint: Option<&str>) -> AvailableCommand {
    let mut command = AvailableCommand::new(name, description);
    if let Some(hint) = hint {
        command = command.input(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new(hint),
        ));
    }
    command
}

pub(crate) async fn available(process: &Process) -> Vec<AvailableCommand> {
    let mut commands = vec![
        command("help", "List commands available through ACP", None),
        command("status", "Show the native session's model and mode", None),
        command(
            "model",
            "Show or select a model from the current provider",
            Some("[model]"),
        ),
        command(
            "mode",
            "Show or select an allowed collaboration mode",
            Some("[mode]"),
        ),
        command("collab", "Alias for /mode", Some("[mode]")),
        command(
            "compact",
            "Summarize the conversation through native compaction",
            None,
        ),
        command(
            "review",
            "Review changes or supplied instructions",
            Some("[instructions]"),
        ),
        command(
            "rename",
            "Rename the durable native session",
            Some("<name>"),
        ),
    ];
    let controls = process.session_controls().await;
    if controls.modes.iter().any(|mode| mode.id == "plan")
        && (controls.switching_allowed || controls.current_mode == "plan")
    {
        commands.push(command(
            "plan",
            "Switch to Plan mode, optionally with a task",
            Some("[task]"),
        ));
    }
    commands.extend(process.custom_prompts().await.into_iter().map(|prompt| {
        command(
            &format!("prompts:{}", prompt.name),
            prompt
                .description
                .as_deref()
                .unwrap_or("Run a stored custom prompt"),
            prompt.argument_hint.as_deref().or(Some("[arguments]")),
        )
    }));
    commands
}

pub(crate) async fn message(
    context: &SessionContext,
    text: impl Into<String>,
) -> protocol::Result<()> {
    context
        .client()
        .update_wait(SessionUpdate::AgentMessageChunk(ContentChunk::new(
            ContentBlock::Text(TextContent::new(text)),
        )))
        .await
}

async fn notice(context: &SessionContext, text: impl Into<String>) -> protocol::Result<Dispatch> {
    message(context, text).await?;
    Ok(Dispatch::Done)
}

async fn select(
    process: &Process,
    context: &SessionContext,
    key: &str,
    value: &str,
) -> protocol::Result<bool> {
    let request = SetSessionConfigOptionRequest::new(
        context.session_id().clone(),
        key.to_owned(),
        SessionConfigOptionValue::ValueId {
            value: value.to_owned().into(),
        },
    );
    match crate::controls::set(process, request).await {
        Ok(_) => {
            crate::controls::publish(process, context).await?;
            Ok(true)
        }
        Err(error) if error.code == protocol::Error::invalid_params().code => {
            message(
                context,
                format!(
                    "Cannot select {key} '{value}': not available under native session policy."
                ),
            )
            .await?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn dispatch(
    process: &Process,
    mut content: Vec<ContentBlock>,
    context: &SessionContext,
) -> protocol::Result<Dispatch> {
    let commands = available(process).await;
    context
        .client()
        .update_wait(SessionUpdate::AvailableCommandsUpdate(
            AvailableCommandsUpdate::new(commands.clone()),
        ))
        .await?;
    let Some(ContentBlock::Text(first)) = content.first() else {
        return Ok(Dispatch::Prompt(content));
    };
    let Some((name, args, _)) = parse_slash_name(&first.text) else {
        return Ok(Dispatch::Prompt(content));
    };
    // Absolute paths, "//", and prose beginning with a path are ordinary input.
    if !name
        .chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | ':'))
    {
        return Ok(Dispatch::Prompt(content));
    }
    let args = args.trim();
    if name.starts_with("prompts:") {
        let prompts = process.custom_prompts().await;
        match expand_custom_prompt(&first.text, &[], &prompts) {
            Ok(Some(expanded)) => {
                content[0] = ContentBlock::Text(TextContent::new(expanded.text));
                return Ok(Dispatch::Prompt(content));
            }
            Ok(None) => {
                return notice(context, "Unknown or disabled custom prompt. Use /help.").await;
            }
            Err(error) => return notice(context, error.user_message()).await,
        }
    }
    if !commands.iter().any(|command| command.name == name) {
        return notice(
            context,
            format!("Command /{name} is not available through ACP. Use /help."),
        )
        .await;
    }
    if content.len() != 1 && !(name == "plan" && !args.is_empty()) {
        return notice(
            context,
            "This command accepts text arguments only; attachments were not executed.",
        )
        .await;
    }
    match name {
        "help" | "status" | "compact" if !args.is_empty() => {
            notice(context, format!("/{name} does not accept arguments.")).await
        }
        "help" => {
            notice(
                context,
                commands
                    .iter()
                    .map(|command| format!("/{} — {}", command.name, command.description))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .await
        }
        "status" => {
            let controls = process.session_controls().await;
            notice(
                context,
                format!("Model: {}\nMode: {}", controls.model, controls.current_mode),
            )
            .await
        }
        "model" | "mode" | "collab" => {
            let key = if name == "model" { "model" } else { "mode" };
            if args.is_empty() {
                let controls = process.session_controls().await;
                let (current, values) = if key == "model" {
                    (
                        controls.model,
                        controls
                            .models
                            .into_iter()
                            .map(|model| model.model)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    (
                        controls.current_mode.clone(),
                        controls
                            .modes
                            .into_iter()
                            .filter(|mode| {
                                controls.switching_allowed || mode.id == controls.current_mode
                            })
                            .map(|mode| mode.id)
                            .collect::<Vec<_>>(),
                    )
                };
                return notice(
                    context,
                    format!("Current {key}: {current}\nAvailable: {}", values.join(", ")),
                )
                .await;
            }
            if select(process, context, key, args).await? {
                message(context, format!("Selected {key}: {args}")).await?;
            }
            Ok(Dispatch::Done)
        }
        "plan" => {
            if !select(process, context, "mode", "plan").await? {
                return Ok(Dispatch::Done);
            }
            if args.is_empty() {
                return notice(context, "Selected mode: plan").await;
            }
            content[0] = ContentBlock::Text(TextContent::new(args));
            Ok(Dispatch::Prompt(content))
        }
        "compact" => Ok(Dispatch::Native(Box::new(Op::Compact))),
        "rename" => {
            let Some(name) = chaos_kern::util::normalize_process_name(args) else {
                return notice(context, "Usage: /rename <name>").await;
            };
            Ok(Dispatch::Native(Box::new(Op::SetProcessName { name })))
        }
        "review" => Ok(Dispatch::Native(Box::new(Op::Review {
            review_request: ReviewRequest {
                target: if args.is_empty() {
                    ReviewTarget::UncommittedChanges
                } else {
                    ReviewTarget::Custom {
                        instructions: args.to_string(),
                    }
                },
                user_facing_hint: None,
                reviewer: None,
            },
        }))),
        _ => notice(context, "Unsupported ACP command. Use /help.").await,
    }
}
