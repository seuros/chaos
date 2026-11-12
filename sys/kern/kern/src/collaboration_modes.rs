use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::TUI_VISIBLE_COLLABORATION_MODES;
use chaos_ipc::openai_models::ReasoningEffort;

const COLLABORATION_MODE_DEFAULT: &str = include_str!("../templates/collaboration_mode/default.md");
const COLLABORATION_MODE_HARNESS: &str = include_str!("../templates/collaboration_mode/harness.md");
const MODE_NAME_PLACEHOLDER: &str = "{{MODE_NAME}}";
const KNOWN_MODE_NAMES_PLACEHOLDER: &str = "{{KNOWN_MODE_NAMES}}";
const REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER: &str = "{{REQUEST_USER_INPUT_AVAILABILITY}}";
const ASKING_QUESTIONS_GUIDANCE_PLACEHOLDER: &str = "{{ASKING_QUESTIONS_GUIDANCE}}";

/// Stores feature flags that control collaboration-mode behavior.
///
/// Keep mode-related flags here so new collaboration-mode capabilities can be
/// added without large cross-cutting diffs to constructor and call-site
/// signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollaborationModesConfig {
    /// Enables `request_user_input` availability in Default and Harness modes.
    pub default_mode_request_user_input: bool,
    /// Exposes Harness mode in debug builds or when launched with `--debug`.
    pub harness_mode: bool,
}

impl Default for CollaborationModesConfig {
    fn default() -> Self {
        Self {
            default_mode_request_user_input: true,
            harness_mode: cfg!(debug_assertions)
                || std::env::var_os("CHAOS_DEBUG_LOG_PATH").is_some(),
        }
    }
}

impl CollaborationModesConfig {
    pub(crate) fn visible_modes(self) -> impl Iterator<Item = ModeKind> {
        TUI_VISIBLE_COLLABORATION_MODES
            .into_iter()
            .filter(move |mode| *mode != ModeKind::Harness || self.harness_mode)
    }
}

pub(crate) fn builtin_collaboration_mode_presets(
    collaboration_modes_config: CollaborationModesConfig,
) -> Vec<CollaborationModeMask> {
    collaboration_modes_config
        .visible_modes()
        .map(|mode| {
            if mode == ModeKind::Plan {
                plan_preset()
            } else {
                execution_preset(mode, collaboration_modes_config)
            }
        })
        .collect()
}

fn plan_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Plan.display_name().to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(plan_mode_instructions(Default::default()))),
    }
}

pub(crate) fn plan_mode_instructions(
    planning: chaos_proc::planning::PlanningCapabilities,
) -> String {
    crate::prompt_template::render_template(
        "collaboration_mode/plan.md",
        minijinja::context!(planning => minijinja::value::Serde(planning)),
    )
}

fn execution_preset(
    mode: ModeKind,
    collaboration_modes_config: CollaborationModesConfig,
) -> CollaborationModeMask {
    CollaborationModeMask {
        name: mode.display_name().to_string(),
        mode: Some(mode),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(execution_mode_instructions(
            mode,
            collaboration_modes_config,
        ))),
    }
}

fn execution_mode_instructions(
    mode: ModeKind,
    collaboration_modes_config: CollaborationModesConfig,
) -> String {
    let modes: Vec<_> = collaboration_modes_config.visible_modes().collect();
    let known_mode_names = format_mode_names(&modes);
    let request_user_input_availability = request_user_input_availability_message(
        mode,
        collaboration_modes_config.default_mode_request_user_input,
    );
    let asking_questions_guidance = asking_questions_guidance_message(
        mode,
        collaboration_modes_config.default_mode_request_user_input,
    );
    let mut instructions = COLLABORATION_MODE_DEFAULT
        .replace(MODE_NAME_PLACEHOLDER, mode.display_name())
        .replace(KNOWN_MODE_NAMES_PLACEHOLDER, &known_mode_names)
        .replace(
            REQUEST_USER_INPUT_AVAILABILITY_PLACEHOLDER,
            &request_user_input_availability,
        )
        .replace(
            ASKING_QUESTIONS_GUIDANCE_PLACEHOLDER,
            &asking_questions_guidance,
        );
    if mode == ModeKind::Harness {
        instructions.push_str(COLLABORATION_MODE_HARNESS);
    }
    instructions
}

fn format_mode_names(modes: &[ModeKind]) -> String {
    let mode_names: Vec<&str> = modes.iter().map(|mode| mode.display_name()).collect();
    match mode_names.as_slice() {
        [] => "none".to_string(),
        [mode_name] => (*mode_name).to_string(),
        [first, second] => format!("{first} and {second}"),
        [..] => mode_names.join(", "),
    }
}

fn request_user_input_availability_message(
    mode: ModeKind,
    default_mode_request_user_input: bool,
) -> String {
    let mode_name = mode.display_name();
    if mode.allows_request_user_input()
        || (default_mode_request_user_input
            && matches!(mode, ModeKind::Default | ModeKind::Harness))
    {
        format!("The `request_user_input` tool is available in {mode_name} mode.")
    } else {
        format!(
            "The `request_user_input` tool is unavailable in {mode_name} mode. \
             If you call it while in {mode_name} mode, it will return an error."
        )
    }
}

fn asking_questions_guidance_message(
    mode: ModeKind,
    default_mode_request_user_input: bool,
) -> String {
    let mode_name = mode.display_name();
    let question_guidance = if default_mode_request_user_input {
        "prefer using the `request_user_input` tool rather than writing a multiple choice \
         question as a textual assistant message."
    } else {
        "ask the user directly with a concise plain-text question."
    };
    format!(
        "In {mode_name} mode, strongly prefer making reasonable assumptions and executing \
         the user's request rather than stopping to ask questions. If you absolutely must \
         ask a question because the answer cannot be discovered from local context and a \
         reasonable assumption would be risky, {question_guidance} Never write a multiple \
         choice question as a textual assistant message."
    )
}

#[cfg(test)]
mod tests;
