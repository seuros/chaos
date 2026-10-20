use acp_host::{SessionContext, SessionInfo, protocol};
use chaos_kern::Process;
use protocol::schema::v1::{
    ConfigOptionUpdate, CurrentModeUpdate, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelect,
    SessionConfigSelectOption, SessionMode, SessionModeState, SessionUpdate,
    SetSessionConfigOptionRequest,
};

pub(crate) async fn info(process: &Process) -> SessionInfo {
    let native = process.session_controls().await;
    let mut models = native
        .models
        .iter()
        .map(|model| {
            SessionConfigSelectOption::new(model.model.clone(), model.display_name.clone())
                .description(model.description.clone())
        })
        .collect::<Vec<_>>();
    if !native
        .models
        .iter()
        .any(|model| model.model == native.model)
    {
        models.insert(
            0,
            SessionConfigSelectOption::new(native.model.clone(), native.model.clone()),
        );
    }
    let modes = native
        .modes
        .iter()
        .filter(|mode| native.switching_allowed || mode.id == native.current_mode)
        .map(|mode| {
            SessionMode::new(mode.id.clone(), mode.title.clone())
                .description(mode.description.clone())
        })
        .collect::<Vec<_>>();
    let options = vec![
        SessionConfigOption::new(
            "model",
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(native.model, models)),
        )
        .category(SessionConfigOptionCategory::Model),
        SessionConfigOption::new(
            "mode",
            "Mode",
            SessionConfigKind::Select(SessionConfigSelect::new(
                native.current_mode.clone(),
                modes
                    .iter()
                    .map(|mode| {
                        SessionConfigSelectOption::new(mode.id.to_string(), mode.name.clone())
                    })
                    .collect::<Vec<_>>(),
            )),
        )
        .category(SessionConfigOptionCategory::Mode),
    ];
    SessionInfo {
        id: None,
        config_options: Some(options),
        modes: Some(SessionModeState::new(native.current_mode, modes)),
    }
}

pub(crate) async fn set(
    process: &Process,
    request: SetSessionConfigOptionRequest,
) -> protocol::Result<SessionInfo> {
    let SessionConfigOptionValue::ValueId { value } = request.value else {
        return Err(crate::invalid("ACP configuration requires a select value"));
    };
    let result = match request.config_id.0.as_ref() {
        "model" => process.set_session_model(value.0.as_ref()).await,
        "mode" => process.set_session_mode(value.0.as_ref()).await,
        _ => return Err(crate::invalid("unknown ACP configuration option")),
    };
    result.map_err(|error| match error {
        chaos_kern::ProcessControlError::Rejected(_) => {
            crate::invalid("native session rejected this configuration change")
        }
        chaos_kern::ProcessControlError::Unconfirmed(_) => {
            crate::internal("native ACP configuration durability is unconfirmed")
        }
    })?;
    Ok(info(process).await)
}

pub(crate) async fn publish(process: &Process, context: &SessionContext) -> protocol::Result<()> {
    let info = info(process).await;
    if let Some(options) = info.config_options {
        context
            .client()
            .update(SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(
                options,
            )))?;
    }
    if let Some(mode) = info.modes {
        context
            .client()
            .update(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                mode.current_mode_id,
            )))?;
    }
    Ok(())
}
