use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::TurnContextItem;

use crate::features::Feature;

use super::Session;
use crate::chaos::TurnContext;

impl Session {
    pub(crate) async fn build_settings_update_items(
        &self,
        reference_context_item: Option<&TurnContextItem>,
        current_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        let previous_turn_settings = {
            let state = self.state.lock().await;
            state.previous_turn_settings()
        };
        let shell = self.user_shell();
        let exec_policy = self.services.exec_policy.current();
        crate::context_manager::updates::build_settings_update_items(
            reference_context_item,
            previous_turn_settings.as_ref(),
            current_context,
            shell.as_ref(),
            exec_policy.as_ref(),
        )
    }

    pub(crate) async fn build_initial_context(
        &self,
        turn_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        let mut developer_sections = Vec::<String>::with_capacity(8);
        let mut contextual_user_sections = Vec::<String>::with_capacity(2);
        let shell = self.user_shell();
        let (
            _reference_context_item,
            previous_turn_settings,
            collaboration_mode,
            base_instructions,
            _session_source,
        ) = {
            let state = self.state.lock().await;
            (
                state.reference_context_item(),
                state.previous_turn_settings(),
                state.session_configuration.collaboration_mode.clone(),
                state.session_configuration.base_instructions.clone(),
                state.session_configuration.session_source.clone(),
            )
        };
        if let Some(model_switch_message) =
            crate::context_manager::updates::build_model_instructions_update_item(
                previous_turn_settings.as_ref(),
                turn_context,
            )
        {
            developer_sections.push(model_switch_message.into_text());
        }
        developer_sections.push(
            crate::developer_instructions::from_policies(
                &turn_context.file_system_sandbox_policy,
                turn_context.network_sandbox_policy,
                turn_context.approval_policy.value(),
                self.services.exec_policy.current().as_ref(),
                &turn_context.cwd,
                turn_context
                    .features
                    .enabled(Feature::ExecPermissionApprovals),
                turn_context
                    .features
                    .enabled(Feature::RequestPermissionsTool),
            )
            .into_text(),
        );
        if let Some(minion_instructions) = turn_context.minion_instructions.as_deref() {
            developer_sections.push(minion_instructions.to_string());
        }
        if let Some(collab_instructions) =
            chaos_ipc::models::DeveloperInstructions::from_collaboration_mode(&collaboration_mode)
        {
            developer_sections.push(collab_instructions.into_text());
        }
        if let Some(personality) = turn_context.personality {
            let model_info = turn_context.model_info.clone();
            let has_baked_personality = model_info.supports_personality()
                && base_instructions == model_info.get_model_instructions(Some(personality));
            if !has_baked_personality
                && let Some(personality_message) =
                    crate::context_manager::updates::personality_message_for(
                        &model_info,
                        personality,
                    )
            {
                developer_sections.push(
                    chaos_ipc::models::DeveloperInstructions::personality_spec_message(
                        personality_message,
                    )
                    .into_text(),
                );
            }
        }
        let subagents = self
            .services
            .agent_control
            .format_environment_context_subagents(self.conversation_id)
            .await;
        contextual_user_sections.push(
            crate::environment_context::EnvironmentContext::from_turn_context(
                turn_context,
                shell.as_ref(),
            )
            .with_subagents(subagents)
            .serialize_to_xml(),
        );

        let mut items = Vec::with_capacity(3);
        if let Some(developer_message) =
            crate::context_manager::updates::build_system_update_item(developer_sections)
        {
            items.push(developer_message);
        }
        if let Some(contextual_user_message) =
            crate::context_manager::updates::build_contextual_user_message(contextual_user_sections)
        {
            items.push(contextual_user_message);
        }
        items
    }
}
