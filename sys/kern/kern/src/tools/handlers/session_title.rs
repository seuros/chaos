use serde::Deserialize;

use crate::chaos::submission_loop::handlers::persist_process_name;
use crate::config::TerminalTitleMode;
use crate::function_tool::FunctionCallError;
use crate::rollout::process_names;
use crate::rollout::process_names::ProcessNameSource;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use super::extract_function_arguments;
use super::parse_arguments;

const MAX_AGENT_SESSION_TITLE_CHARS: usize = 80;

pub struct SessionTitleHandler;

#[derive(Debug, Deserialize)]
struct Args {
    title: String,
}

impl ToolHandler for SessionTitleHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            ..
        } = invocation;
        if turn.config.terminal_title != TerminalTitleMode::Agent {
            return Err(FunctionCallError::RespondToModel(
                "agent-managed session titles are disabled for this session".to_string(),
            ));
        }

        let arguments = extract_function_arguments(payload, &tool_name)?;
        let args: Args = parse_arguments(&arguments)?;
        let title = crate::util::normalize_process_name(&args.title).ok_or_else(|| {
            FunctionCallError::RespondToModel("session title cannot be empty".to_string())
        })?;
        if title.chars().count() > MAX_AGENT_SESSION_TITLE_CHARS {
            return Err(FunctionCallError::RespondToModel(format!(
                "session title must be at most {MAX_AGENT_SESSION_TITLE_CHARS} characters"
            )));
        }

        if let Some(existing) =
            process_names::find_process_name_record_by_id(&session.conversation_id)
                .await
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
        {
            if existing.name == title {
                return Ok(FunctionToolOutput::from_text(
                    format!("Session title is already `{title}`."),
                    Some(true),
                ));
            }
            if existing.source == ProcessNameSource::User {
                return Err(FunctionCallError::RespondToModel(
                    "the user explicitly named this session; their title cannot be replaced"
                        .to_string(),
                ));
            }
        }

        if let Some(other_process_id) = process_names::find_unarchived_process_id_by_name(&title)
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            && other_process_id != session.conversation_id
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "another active session is already named `{title}`; choose a more distinctive title"
            )));
        }

        persist_process_name(
            &session,
            turn.sub_id.clone(),
            title.clone(),
            ProcessNameSource::Agent,
        )
        .await
        .map_err(FunctionCallError::RespondToModel)?;

        Ok(FunctionToolOutput::from_text(
            format!("Session title set to `{title}`."),
            Some(true),
        ))
    }
}
