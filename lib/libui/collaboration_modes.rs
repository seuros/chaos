use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ModeKind;
use chaos_kern::models_manager::manager::ModelsManager;

fn filtered_presets(models_manager: &ModelsManager) -> Vec<CollaborationModeMask> {
    models_manager
        .list_collaboration_modes()
        .into_iter()
        .filter(|mask| mask.mode.is_some_and(ModeKind::is_tui_visible))
        .collect()
}

pub fn presets_for_tui(models_manager: &ModelsManager) -> Vec<CollaborationModeMask> {
    filtered_presets(models_manager)
}

pub fn default_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(models_manager);
    presets
        .iter()
        .find(|mask| mask.mode == Some(ModeKind::Default))
        .cloned()
        .or_else(|| presets.into_iter().next())
}

pub fn mask_for_kind(
    models_manager: &ModelsManager,
    kind: ModeKind,
) -> Option<CollaborationModeMask> {
    if !kind.is_tui_visible() {
        return None;
    }
    filtered_presets(models_manager)
        .into_iter()
        .find(|mask| mask.mode == Some(kind))
}

/// Cycle to the next collaboration mode preset in list order.
pub fn next_mask(
    models_manager: &ModelsManager,
    current: Option<&CollaborationModeMask>,
) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(models_manager);
    if presets.is_empty() {
        return None;
    }
    let current_kind = current.and_then(|mask| mask.mode);
    let next_index = presets
        .iter()
        .position(|mask| mask.mode == current_kind)
        .map_or(0, |idx| (idx + 1) % presets.len());
    presets.get(next_index).cloned()
}

pub fn default_mode_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    mask_for_kind(models_manager, ModeKind::Default)
}

pub fn plan_mask(models_manager: &ModelsManager) -> Option<CollaborationModeMask> {
    mask_for_kind(models_manager, ModeKind::Plan)
}
