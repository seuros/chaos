use super::*;
use crate::chatwidget::tests::make_chatwidget_manual;
use chaos_ipc::openai_models::ReasoningEffortPreset;
use chaos_kern::models_manager::catalog::ModelSelection;
use crossterm::event::KeyEvent;
use std::assert_matches;
use tokio::sync::mpsc::UnboundedReceiver;

pub(super) async fn run() {
    account_model_selection().await;
    picker_entry_points_share_visibility_and_deduplication().await;
    quick_auto_selection_preserves_the_account().await;
    reasoning_and_rate_limit_preserve_the_account().await;
    qualified_lower_cost_selection_does_not_offer_itself().await;
}

fn account_choice(provider: &str, model: &str) -> ModelPreset {
    let mut preset: ModelPreset = chaos_kern::test_support::test_model_info(model).into();
    preset.id = ModelSelection {
        provider_id: provider.into(),
        model: preset.model.clone(),
    }
    .id();
    preset.display_name = format!("{model} · {provider}");
    preset.default_reasoning_effort = ReasoningEffortConfig::Medium;
    preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::Medium,
        description: String::new(),
    }];
    preset
}

fn take_events(events: &mut UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

fn render_picker(chat: &ChatWidget, width: u16) -> String {
    crate::test_render::render_to_trimmed_string(
        chat,
        Rect::new(0, 0, width, chat.desired_height(width)),
    )
}

async fn account_model_selection() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(Some("shared-model")).await;
    chat.process_id = Some(ProcessId::new());
    chat.config.model_provider_id = "first".into();
    chat.config
        .model_providers
        .insert("second".into(), chat.config.model_provider.clone());
    while events.try_recv().is_ok() {}
    let first = account_choice("first", "shared-model");
    let second = account_choice("second", "shared-model");
    assert!(chat.model_choice_is_current(&first.id));
    assert!(!chat.model_choice_is_current(&second.id));
    chat.open_model_popup_with_presets(vec![first.clone(), second.clone()]);
    let popup = render_picker(&chat, 90);
    assert!(popup.contains("shared-model · first"), "{popup}");
    assert!(popup.contains("shared-model · second"), "{popup}");
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let selected = take_events(&mut events)
        .into_iter()
        .find_map(|event| match event {
            AppEvent::OpenReasoningPopup { model } => Some(model),
            _ => None,
        })
        .expect("model choice opens reasoning with its account identity");
    assert_eq!(selected.id, second.id);
    chat.open_reasoning_popup(selected);
    let emitted = take_events(&mut events);
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, AppEvent::UpdateModel(id) if id == &second.id))
    );
    assert!(emitted.iter().any(|event| matches!(
        event,
        AppEvent::PersistModelSelection { model, .. } if model == &second.id
    )));
    chat.set_model(&second.id);
    assert_eq!(chat.config.model_provider_id, "second");
    assert!(chat.model_choice_is_current(&second.id));
    assert!(!chat.model_choice_is_current(&first.id));
    assert_eq!(chat.model_display_name(), "shared-model");
}

async fn picker_entry_points_share_visibility_and_deduplication() {
    for all_models in [false, true] {
        let (mut chat, _events, _ops) = make_chatwidget_manual(Some("shared-model")).await;
        chat.config.model_provider_id = "first".into();
        let first = account_choice("first", "shared-model");
        let second = account_choice("second", "shared-model");
        let mut hidden = account_choice("first", "hidden-model");
        hidden.show_in_picker = false;
        let presets = vec![first.clone(), hidden, first, second];
        if all_models {
            chat.open_all_models_popup(presets);
        } else {
            chat.open_model_popup_with_presets(presets);
        }
        let popup = render_picker(&chat, 100);
        assert_eq!(popup.matches("shared-model · first").count(), 1, "{popup}");
        assert_eq!(popup.matches("shared-model · second").count(), 1, "{popup}");
        assert!(!popup.contains("hidden-model"), "{popup}");
    }
}

async fn quick_auto_selection_preserves_the_account() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(Some("shared-model")).await;
    chat.config.model_provider_id = "first".into();
    let first = account_choice("first", "chaos-auto-fast");
    let second = account_choice("second", "chaos-auto-fast");
    while events.try_recv().is_ok() {}
    chat.open_model_popup_with_presets(vec![first.clone(), first, second.clone()]);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let emitted = take_events(&mut events);
    assert!(emitted.iter().any(|event| matches!(
        event,
        AppEvent::PersistModelSelection { model, .. } if model == &second.id
    )));
}

async fn reasoning_and_rate_limit_preserve_the_account() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(Some("shared-model")).await;
    chat.config.model_provider_id = "first".into();
    let mut second = account_choice("second", "shared-model");
    second
        .supported_reasoning_efforts
        .push(ReasoningEffortPreset {
            effort: ReasoningEffortConfig::High,
            description: String::new(),
        });
    while events.try_recv().is_ok() {}
    chat.open_reasoning_popup(second.clone());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let emitted = take_events(&mut events);
    assert!(emitted.iter().any(|event| matches!(
        event,
        AppEvent::PersistModelSelection { model, .. } if model == &second.id
    )));

    chat.open_rate_limit_switch_prompt(second.clone());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let emitted = take_events(&mut events);
    assert!(emitted.iter().any(|event| matches!(
        event,
        AppEvent::ChaosOp(Op::OverrideTurnContext { model: Some(id), .. }) if id == &second.id
    )));
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, AppEvent::UpdateModel(id) if id == &second.id))
    );
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, AppEvent::PersistModelSelection { .. })),
        "rate-limit choices stay session-local"
    );
}

async fn qualified_lower_cost_selection_does_not_offer_itself() {
    use chaos_ipc::protocol::{RateLimitSnapshot, RateLimitWindow};
    let (mut chat, _events, _ops) = make_chatwidget_manual(Some(NUDGE_MODEL_SLUG)).await;
    let preset = account_choice(&chat.config.model_provider_id, NUDGE_MODEL_SLUG);
    chat.set_model(&preset.id);
    chat.auth_manager = chaos_kern::test_support::auth_manager_from_auth(
        chaos_kern::ChaosAuth::create_dummy_chatgpt_auth_for_testing(),
    );
    chat.on_rate_limit_snapshot(Some(RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 95.0,
            window_minutes: Some(60),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        plan_type: None,
    }));
    assert!(chat.model_choice_is_current(NUDGE_MODEL_SLUG));
    assert_matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Idle
    );
}
