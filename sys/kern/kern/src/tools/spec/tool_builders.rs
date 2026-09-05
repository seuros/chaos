use std::collections::BTreeMap;

use chaos_ipc::models::VIEW_IMAGE_TOOL_NAME;
use chaos_ipc::openai_models::ModelPreset;
use chaos_parrot::sanitize::JsonSchema;
use chaos_parrot::sanitize::ResponsesApiTool;

use crate::child_agents::tools::DEFAULT_WAIT_TIMEOUT_MS;
use crate::child_agents::tools::MAX_WAIT_TIMEOUT_MS;
use crate::child_agents::tools::MIN_WAIT_TIMEOUT_MS;
use crate::client_common::tools::ToolSpec;
use crate::collaboration_modes::CollaborationModesConfig;
use crate::tools::handlers::request_permissions_tool_description;
use crate::tools::handlers::request_user_input_tool_description;
use mcp_host::prelude::ToolGroupCatalog;

use super::ToolsConfig;
use super::schemas::{
    close_agent_output_schema, create_approval_parameters, create_request_permissions_schema,
    resume_agent_output_schema, send_input_output_schema, spawn_agent_output_schema,
    unified_exec_output_schema, wait_output_schema,
};

pub(crate) fn create_tool_group_control_tool(name: &str, catalog: &ToolGroupCatalog) -> ToolSpec {
    let definitions = catalog.definitions();
    let available = definitions
        .iter()
        .map(|group| format!("{}: {}", group.id, group.description))
        .collect::<Vec<_>>()
        .join("\n");
    let action = if name == "enable_tools" {
        "Enable"
    } else {
        "Disable"
    };
    let properties = BTreeMap::from([(
        "groups".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(format!(
                "One or more capability group IDs. Available groups:\n{available}"
            )),
        },
    )]);
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: format!(
            "{action} one or more capability groups for this session. The next model sample sees the updated tool set."
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["groups".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_switch_mode_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "mode_id".to_string(),
        JsonSchema::String {
            description: Some(
                "Target mode id from the caller-filtered chaos://modes resource.".to_string(),
            ),
        },
    )]);
    ToolSpec::Function(ResponsesApiTool {
        name: "switch_mode".to_string(),
        description: "Switch this ChaOS session to another allowed mode. Read chaos://modes for the caller-filtered catalog. The next model sample in the same user turn uses the new mode. The switch is session-scoped and cannot change parent, sibling, or global modes."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["mode_id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_compaction_control_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Action to request. Use `compact_now` to compact at the next safe turn-loop boundary, or `defer_once` to extend only the current pressure window to Chaos's fixed safety ceiling."
                        .to_string(),
                ),
            },
        ),
        (
            "window_id".to_string(),
            JsonSchema::String {
                description: Some(
                    "The current compaction_reflex window_id. Required for `defer_once`; optional for `compact_now`, but if supplied it must be current."
                        .to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "compaction_control".to_string(),
        description: "Exercise bounded control over this session's automatic compaction timing. `defer_once` is available only after the current compaction reflex, cannot stack, and never overrides Chaos's fixed safety ceiling. `compact_now` may be requested at any time; when agent-managed titles are available, review whether the current session title is still accurate before requesting it. Doing nothing means continue with normal automatic compaction."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_set_session_title_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "title".to_string(),
        JsonSchema::String {
            description: Some(
                "A short, concrete, distinctive 2-6 word title naming the session's primary work."
                    .to_string(),
            ),
        },
    )]);
    ToolSpec::Function(ResponsesApiTool {
        name: "set_session_title".to_string(),
        description: "Name the current Chaos session so the user can identify its terminal tab and resume entry. Use this once the primary work is clear, and update it only when the session's purpose materially changes. Choose a distinctive title another session would not plausibly share. User-authored names cannot be replaced."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["title".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_read_session_history_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "before_seq".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Exclusive sequence cursor returned by an earlier call. When omitted, reads immediately before the latest compaction, or from the journal end if no compaction has occurred."
                        .to_string(),
                ),
            },
        ),
        (
            "max_items".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Maximum transcript entries to return. Defaults to 40 and is capped at 100."
                        .to_string(),
                ),
            },
        ),
        (
            "max_bytes".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Approximate maximum transcript text bytes to return. Defaults to 24000 and is capped at 64000."
                        .to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "read_session_history".to_string(),
        description: "Read a bounded page of this agent's own canonical session transcript. By default it opens immediately before the latest compaction, preserving journal sequence provenance while omitting hidden reasoning, encrypted payloads, images, and telemetry. Use next_before_seq to page farther back."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_search_session_history_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::String {
                description: Some(
                    "Literal text to find in this agent's canonical session transcript."
                        .to_string(),
                ),
            },
        ),
        (
            "before_seq".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Exclusive sequence cursor returned by an earlier search. When omitted, searches the whole journal."
                        .to_string(),
                ),
            },
        ),
        (
            "max_results".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Maximum matching entries to return. Defaults to 20 and is capped at 50."
                        .to_string(),
                ),
            },
        ),
        (
            "max_bytes".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Approximate maximum excerpt bytes to return. Defaults to 24000 and is capped at 64000."
                        .to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "search_session_history".to_string(),
        description: "Search this agent's own canonical persisted session transcript using bounded literal matching (ASCII case-insensitive; other text exact). Results are newest first and carry journal sequence numbers; hidden reasoning, encrypted payloads, images, and telemetry are omitted."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["query".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_set_parent_effort_tool() -> ToolSpec {
    let levels = "none, minimal, low, medium, high, xhigh, max, ultra";
    let properties = BTreeMap::from([
        (
            "effort".to_string(),
            JsonSchema::String {
                description: Some(format!(
                    "Reasoning effort for subsequent parent turns. One of: {levels}."
                )),
            },
        ),
        (
            "reason".to_string(),
            JsonSchema::String {
                description: Some(
                    "Brief user-visible reason for changing effort (optional).".to_string(),
                ),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "set_parent_effort".to_string(),
        description: "Changes this parent session's reasoning effort for subsequent turns. It cannot change the effort of the turn already in progress."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["effort".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_exec_command_tool(
    allow_login_shell: bool,
    exec_permission_approvals_enabled: bool,
) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "cmd".to_string(),
            JsonSchema::String {
                description: Some("Shell command to execute.".to_string()),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional working directory to run the command in; defaults to the turn cwd."
                        .to_string(),
                ),
            },
        ),
        (
            "shell".to_string(),
            JsonSchema::String {
                description: Some("Shell binary to launch. Defaults to the user's default shell.".to_string()),
            },
        ),
        (
            "tty".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to allocate a TTY for the command. Defaults to false (plain pipes); set to true to open a PTY and access TTY process."
                        .to_string(),
                ),
            }
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_string(),
                ),
            },
        ),
        (
            "max_output_tokens".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated."
                        .to_string(),
                ),
            },
        ),
    ]);
    if allow_login_shell {
        properties.insert(
            "login".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with -l/-i semantics. Defaults to true.".to_string(),
                ),
            },
        );
    }
    properties.extend(create_approval_parameters(
        exec_permission_approvals_enabled,
    ));

    ToolSpec::Function(ResponsesApiTool {
        name: "exec_command".to_string(),
        description:
            "Runs a command in a PTY, returning output plus a task ID for lifecycle tracking and, when still running, a session ID for ongoing interaction."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["cmd".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(unified_exec_output_schema()),
    })
}

pub(crate) fn create_write_stdin_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_string(),
            JsonSchema::Integer {
                description: Some("Identifier of the running unified exec session.".to_string()),
            },
        ),
        (
            "chars".to_string(),
            JsonSchema::String {
                description: Some("Bytes to write to stdin (may be empty to poll).".to_string()),
            },
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_string(),
                ),
            },
        ),
        (
            "max_output_tokens".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "write_stdin".to_string(),
        description:
            "Writes characters to an existing unified exec session and returns recent output."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(unified_exec_output_schema()),
    })
}

pub(crate) fn create_shell_tool(exec_permission_approvals_enabled: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("The command to execute".to_string()),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_string()),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Integer {
                description: Some("The timeout for the command in milliseconds".to_string()),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(
        exec_permission_approvals_enabled,
    ));

    let description = r#"Runs a shell command and returns its output.
- The arguments to `shell` will be passed to execvp(). Most terminal commands should be prefixed with ["bash", "-lc"].
- Always set the `workdir` param when using the shell function. Do not use `cd` unless absolutely necessary."#.to_string();

    ToolSpec::Function(ResponsesApiTool {
        name: "shell".to_string(),
        description,
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_shell_command_tool(
    allow_login_shell: bool,
    exec_permission_approvals_enabled: bool,
) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::String {
                description: Some(
                    "The shell script to execute in the user's default shell".to_string(),
                ),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_string()),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Integer {
                description: Some("The timeout for the command in milliseconds".to_string()),
            },
        ),
    ]);
    if allow_login_shell {
        properties.insert(
            "login".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with login shell semantics. Defaults to true."
                        .to_string(),
                ),
            },
        );
    }
    properties.extend(create_approval_parameters(
        exec_permission_approvals_enabled,
    ));

    let description = r#"Runs a shell command and returns its output.
- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary."#.to_string();

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_command".to_string(),
        description,
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_view_image_tool(can_request_original_image_detail: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([(
        "path".to_string(),
        JsonSchema::String {
            description: Some("Local filesystem path to an image file".to_string()),
        },
    )]);
    if can_request_original_image_detail {
        properties.insert(
            "detail".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional detail override. The only supported value is `original`; omit this field for default resized behavior. Use `original` to preserve the file's original resolution instead of resizing to fit. This is important when high-fidelity image perception or precise localization is needed, especially for CUA agents.".to_string(),
                ),
            },
        );
    }

    ToolSpec::Function(ResponsesApiTool {
        name: VIEW_IMAGE_TOOL_NAME.to_string(),
        description: "View a local image from the filesystem (only use if given a full filepath by the user, and the image isn't already attached to the thread context within <image ...> tags)."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["path".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_collab_input_items_schema() -> JsonSchema {
    let properties = BTreeMap::from([
        (
            "type".to_string(),
            JsonSchema::String {
                description: Some(
                    "Input item type: text, image, local_image, skill, or mention.".to_string(),
                ),
            },
        ),
        (
            "text".to_string(),
            JsonSchema::String {
                description: Some("Text content when type is text.".to_string()),
            },
        ),
        (
            "image_url".to_string(),
            JsonSchema::String {
                description: Some("Image URL when type is image.".to_string()),
            },
        ),
        (
            "path".to_string(),
            JsonSchema::String {
                description: Some(
                    "Path when type is local_image/skill, or structured mention target such as app://<connector-id> when type is mention."
                        .to_string(),
                ),
            },
        ),
        (
            "name".to_string(),
            JsonSchema::String {
                description: Some("Display name when type is skill or mention.".to_string()),
            },
        ),
    ]);

    JsonSchema::Array {
        items: Box::new(JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        }),
        description: Some(
            "Structured input items. Use this to pass explicit mentions (for example app:// connector paths)."
                .to_string(),
        ),
    }
}

pub(crate) fn create_spawn_agent_tool(config: &ToolsConfig) -> ToolSpec {
    let available_models_description = spawn_agent_models_description(&config.available_models);
    let properties = BTreeMap::from([
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some(
                    "Initial plain-text task for the new agent. Use either message or items."
                        .to_string(),
                ),
            },
        ),
        ("items".to_string(), create_collab_input_items_schema()),
        (
            "agent_type".to_string(),
            JsonSchema::String {
                description: Some(crate::child_agents::role::spawn_tool_spec::build(
                    &config.agent_roles,
                )),
            },
        ),
        (
            "topics".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    "Topic tags for dynamic role routing (e.g. [\"ruby\", \"rails\"]). \
                     The kernel selects a matching persona at random and emits its catchphrase. \
                     Ignored when `agent_type` is set. \
                     Unmatched topics are surfaced to the user as a warning."
                        .to_string(),
                ),
            },
        ),
        (
            "fork_context".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, fork the current thread history into the new agent before sending the initial prompt. This must be used when you want the new agent to have exactly the same context as you."
                        .to_string(),
                ),
            },
        ),
        (
            "model_provider".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional configured model provider/account id for the new agent. The host \
                     validates the selected model against that provider's cached catalog and \
                     fails closed when the provider cannot be rebound."
                        .to_string(),
                ),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional model override for the new agent. Replaces the inherited model."
                        .to_string(),
                ),
            },
        ),
        (
            "reasoning_effort".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional reasoning effort override for the new agent. Replaces the inherited reasoning effort."
                        .to_string(),
                ),
            },
        ),
        (
            "mode".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional initial mode id for the child. When allowed_modes is omitted, this creates a fixed single-mode child with no switch_mode tool. Use mode `plan` for a fixed planner child agent."
                        .to_string(),
                ),
            },
        ),
        (
            "allowed_modes".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    "Optional child-visible mode catalog. It must be a subset of the caller's catalog and cannot elevate capabilities beyond the active parent mode."
                        .to_string(),
                ),
            },
        ),
        (
            "allow_mode_switching".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether the child may switch among allowed_modes. The kernel rejects switching with fewer than two allowed modes."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "spawn_agent".to_string(),
        description: format!(
            r#"
        Only use `spawn_agent` if and only if the user explicitly asks for sub-agents, delegation, or parallel agent work.
        Requests for depth, thoroughness, research, investigation, or detailed codebase analysis do not count as permission to spawn.
        Agent-role guidance below only helps choose which agent to use after spawning is already authorized; it never authorizes spawning by itself.
        Spawn a sub-agent for a well-scoped task. Returns the agent id, task id, and user-facing nickname when available. This spawn_agent tool provides you access to smaller but more efficient sub-agents. A mini model can solve many tasks faster than the main model. You should follow the rules and guidelines below to use this tool.

{available_models_description}
### When to delegate vs. do the subtask yourself
- First, quickly analyze the overall user task and form a succinct high-level plan. Identify which tasks are immediate blockers on the critical path, and which tasks are sidecar tasks that are needed but can run in parallel without blocking the next local step. As part of that plan, explicitly decide what immediate task you should do locally right now. Do this planning step before delegating to agents so you do not hand off the immediate blocking task to a submodel and then waste time waiting on it.
- Use the smaller subagent when a subtask is easy enough for it to handle and can run in parallel with your local work. Prefer delegating concrete, bounded sidecar tasks that materially advance the main task without blocking your immediate next local step.
- Do not delegate urgent blocking work when your immediate next step depends on that result. If the very next action is blocked on that task, the main rollout should usually do it locally to keep the critical path moving.
- Keep work local when the subtask is too difficult to delegate well and when it is tightly coupled, urgent, or likely to block your immediate next step.

### Designing delegated subtasks
- Subtasks must be concrete, well-defined, and self-contained.
- Delegated subtasks must materially advance the main task.
- Do not duplicate work between the main rollout and delegated subtasks.
- Avoid issuing multiple delegate calls on the same unresolved thread unless the new delegated task is genuinely different and necessary.
- Narrow the delegated ask to the concrete output you need next.
        - For coding tasks, prefer delegating concrete code-change task subtasks over read-only scout analysis when the subagent can make a bounded patch in a clear write scope.
- When delegating coding work, instruct the submodel to edit files directly in its forked workspace and list the file paths it changed in the final answer.
- For code-edit subtasks, decompose work so each delegated task has a disjoint write set.

### After you delegate
- Call wait_agent very sparingly. Only call wait_agent when you need the result immediately for the next critical-path step and you are blocked until it returns.
- Do not redo delegated subagent tasks yourself; focus on integrating results or tackling non-overlapping work.
- While the subagent is running in the background, do meaningful non-overlapping work immediately.
- Do not repeatedly wait by reflex.
- When a delegated coding task returns, quickly review the uploaded changes, then integrate or refine them.

### Parallel delegation patterns
- Run multiple independent information-seeking subtasks in parallel when you have distinct questions that can be answered independently.
- Split implementation into disjoint codebase slices and spawn multiple agents for them in parallel when the write scopes do not overlap.
- Delegate verification only when it can run in parallel with ongoing implementation and is likely to catch a concrete risk before final integration.
- The key is to find opportunities to spawn multiple independent subtasks in parallel within the same round, while ensuring each subtask is well-defined, self-contained, and materially advances the main task."#
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: Some(spawn_agent_output_schema()),
    })
}

pub(crate) fn create_run_synopsis_tool(config: &ToolsConfig) -> ToolSpec {
    let available_roles = crate::child_agents::role::spawn_tool_spec::build(&config.agent_roles);
    let job_properties = BTreeMap::from([
        (
            "id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Synopsis-wide unique job id used to correlate the result.".to_string(),
                ),
            },
        ),
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some("Plain-text task for this agent.".to_string()),
            },
        ),
        (
            "agent_type".to_string(),
            JsonSchema::String {
                description: Some(available_roles),
            },
        ),
    ]);
    let properties = BTreeMap::from([
        (
            "mode".to_string(),
            JsonSchema::String {
                description: Some(
                    "Control-flow mode: sequence, parallel_all, fallback, or race.".to_string(),
                ),
            },
        ),
        (
            "jobs".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::Object {
                    properties: job_properties,
                    required: Some(vec!["id".to_string(), "message".to_string()]),
                    additional_properties: Some(false.into()),
                }),
                description: Some(
                    "One to sixteen agent jobs. Each id must be unique within the synopsis."
                        .to_string(),
                ),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Overall timeout in milliseconds. Defaults to 1800000 and is clamped between 10000 and 3600000."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "run_synopsis".to_string(),
        description: "Only use `run_synopsis` when the user explicitly authorizes sub-agents, delegation, or parallel agent work. Execute a small behavior-tree workflow over real ChaOS agents. `sequence` runs jobs in order and stops on failure; `parallel_all` requires every job to succeed; `fallback` tries jobs until one succeeds; `race` returns the first terminal result and cancels the rest. The call blocks until the workflow terminates, returns each agent's final status/message, and automatically closes all agents it spawned."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["mode".to_string(), "jobs".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn spawn_agent_models_description(models: &[ModelPreset]) -> String {
    let visible_models: Vec<&ModelPreset> =
        models.iter().filter(|model| model.show_in_picker).collect();
    if visible_models.is_empty() {
        return "No picker-visible models are currently loaded.".to_string();
    }

    visible_models
        .into_iter()
        .map(|model| {
            let efforts = model
                .supported_reasoning_efforts
                .iter()
                .map(|preset| format!("{} ({})", preset.effort, preset.description))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "- {} (`{}`): {} Default reasoning effort: {}. Supported reasoning efforts: {}.",
                model.display_name,
                model.model,
                model.description,
                model.default_reasoning_effort,
                efforts
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn create_spawn_child_agents_on_csv_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "csv_path".to_string(),
        JsonSchema::String {
            description: Some("Path to the CSV file containing input rows.".to_string()),
        },
    );
    properties.insert(
        "instruction".to_string(),
        JsonSchema::String {
            description: Some(
                "Instruction template to apply to each CSV row. Use {column_name} placeholders to inject values from the row."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "id_column".to_string(),
        JsonSchema::String {
            description: Some("Optional column name to use as stable item id.".to_string()),
        },
    );
    properties.insert(
        "output_csv_path".to_string(),
        JsonSchema::String {
            description: Some("Optional output CSV path for exported results.".to_string()),
        },
    );
    properties.insert(
        "max_concurrency".to_string(),
        JsonSchema::Integer {
            description: Some(
                "Maximum concurrent tasks for this job. Defaults to 16 and is capped by config."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "max_workers".to_string(),
        JsonSchema::Integer {
            description: Some(
                "Alias for max_concurrency. Set to 1 to run sequentially.".to_string(),
            ),
        },
    );
    properties.insert(
        "max_runtime_seconds".to_string(),
        JsonSchema::Integer {
            description: Some(
                "Maximum runtime per task before it is failed. Defaults to 1800 seconds."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "output_schema".to_string(),
        JsonSchema::Object {
            properties: BTreeMap::new(),
            required: None,
            additional_properties: None,
        },
    );
    ToolSpec::Function(ResponsesApiTool {
        name: "spawn_child_agents_on_csv".to_string(),
        description: "Process a CSV by spawning one task sub-agent per row. The instruction string is a template where `{column}` placeholders are replaced with row values. Each task must call `report_child_agent_job_result` with a JSON object (matching `output_schema` when provided); missing reports are treated as failures. This call blocks until all rows finish and automatically exports results to `output_csv_path` (or a default path)."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["csv_path".to_string(), "instruction".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_report_child_agent_job_result_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "job_id".to_string(),
        JsonSchema::String {
            description: Some("Identifier of the job.".to_string()),
        },
    );
    properties.insert(
        "item_id".to_string(),
        JsonSchema::String {
            description: Some("Identifier of the job item.".to_string()),
        },
    );
    properties.insert(
        "result".to_string(),
        JsonSchema::Object {
            properties: BTreeMap::new(),
            required: None,
            additional_properties: None,
        },
    );
    properties.insert(
        "stop".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Optional. When true, cancels the remaining job items after this result is recorded."
                    .to_string(),
            ),
        },
    );
    ToolSpec::Function(ResponsesApiTool {
        name: "report_child_agent_job_result".to_string(),
        description:
            "Worker-only tool to report a result for a child agent job item. Main agents should not call this."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "job_id".to_string(),
                "item_id".to_string(),
                "result".to_string(),
            ]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_send_input_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "id".to_string(),
            JsonSchema::String {
                description: Some("Agent id to message (from spawn_agent).".to_string()),
            },
        ),
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some(
                    "Legacy plain-text message to send to the agent. Use either message or items."
                        .to_string(),
                ),
            },
        ),
        ("items".to_string(), create_collab_input_items_schema()),
        (
            "interrupt".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, stop the agent's current task and handle this immediately. When false (default), queue this message."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "send_input".to_string(),
        description: "Send a message to an existing agent. Use interrupt=true to redirect work immediately. You should reuse the agent by send_input if you believe your assigned task is highly dependent on the context of a previous task."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(send_input_output_schema()),
    })
}

pub(crate) fn create_send_to_supervisor_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some(
                    "Plain-text message to send to your supervisor. Use either message or items."
                        .to_string(),
                ),
            },
        ),
        ("items".to_string(), create_collab_input_items_schema()),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "send_to_supervisor".to_string(),
        description: "Send a message to your supervisor. Use it for important progress, blockers, questions, or early findings. The message is queued without interrupting the supervisor's current turn. Your final response is returned automatically, so do not use this tool merely to repeat it."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: Some(send_input_output_schema()),
    })
}

pub(crate) fn create_resume_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Agent id to resume.".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "resume_agent".to_string(),
        description:
            "Resume a previously closed agent by id so it can receive send_input and wait_agent calls."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(resume_agent_output_schema()),
    })
}

pub(crate) fn create_wait_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "ids".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "Agent ids to wait on. Pass multiple ids to wait for whichever finishes first."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "timeout_ms".to_string(),
        JsonSchema::Integer {
            description: Some(format!(
                "Optional timeout in milliseconds. Defaults to {DEFAULT_WAIT_TIMEOUT_MS}, min {MIN_WAIT_TIMEOUT_MS}, max {MAX_WAIT_TIMEOUT_MS}. Prefer longer waits (minutes) to avoid busy polling."
            )),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "wait_agent".to_string(),
        description: "Wait for agents to reach a final status. Completed statuses may include the agent's final message. Returns empty status when timed out. Once the agent reaches a final status, a notification message will be received containing the same completed status."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["ids".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(wait_output_schema()),
    })
}

pub(crate) fn create_request_user_input_tool(
    collaboration_modes_config: CollaborationModesConfig,
) -> ToolSpec {
    let mut option_props = BTreeMap::new();
    option_props.insert(
        "label".to_string(),
        JsonSchema::String {
            description: Some("User-facing label (1-5 words).".to_string()),
        },
    );
    option_props.insert(
        "description".to_string(),
        JsonSchema::String {
            description: Some(
                "One short sentence explaining impact/tradeoff if selected.".to_string(),
            ),
        },
    );

    let options_schema = JsonSchema::Array {
        description: Some(
            "Provide 2-3 mutually exclusive choices. Put the recommended option first and suffix its label with \"(Recommended)\". Do not include an \"Other\" option in this list; the client will add a free-form \"Other\" option automatically."
                .to_string(),
        ),
        items: Box::new(JsonSchema::Object {
            properties: option_props,
            required: Some(vec!["label".to_string(), "description".to_string()]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut question_props = BTreeMap::new();
    question_props.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Stable identifier for mapping answers (snake_case).".to_string()),
        },
    );
    question_props.insert(
        "header".to_string(),
        JsonSchema::String {
            description: Some(
                "Short header label shown in the UI (12 or fewer chars).".to_string(),
            ),
        },
    );
    question_props.insert(
        "question".to_string(),
        JsonSchema::String {
            description: Some("Single-sentence prompt shown to the user.".to_string()),
        },
    );
    question_props.insert("options".to_string(), options_schema);

    let questions_schema = JsonSchema::Array {
        description: Some("Questions to show the user. Prefer 1 and do not exceed 3".to_string()),
        items: Box::new(JsonSchema::Object {
            properties: question_props,
            required: Some(vec![
                "id".to_string(),
                "header".to_string(),
                "question".to_string(),
                "options".to_string(),
            ]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut properties = BTreeMap::new();
    properties.insert("questions".to_string(), questions_schema);

    ToolSpec::Function(ResponsesApiTool {
        name: "request_user_input".to_string(),
        description: request_user_input_tool_description(
            collaboration_modes_config.default_mode_request_user_input,
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["questions".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_request_permissions_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "reason".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional short explanation for why additional permissions are needed.".to_string(),
            ),
        },
    );
    properties.insert(
        "permissions".to_string(),
        create_request_permissions_schema(),
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "request_permissions".to_string(),
        description: request_permissions_tool_description(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["permissions".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_close_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Agent id to close (from spawn_agent).".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "close_agent".to_string(),
        description: "Close an agent when it is no longer needed and return its last known status. Don't keep agents open for too long if they are not needed anymore.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: Some(close_agent_output_schema()),
    })
}

pub(crate) fn create_test_sync_tool() -> ToolSpec {
    let barrier_properties = BTreeMap::from([
        (
            "id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Identifier shared by concurrent calls that should rendezvous".to_string(),
                ),
            },
        ),
        (
            "participants".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Number of tool calls that must arrive before the barrier opens".to_string(),
                ),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Maximum time in milliseconds to wait at the barrier".to_string(),
                ),
            },
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "sleep_before_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Optional delay in milliseconds before any other action".to_string(),
                ),
            },
        ),
        (
            "sleep_after_ms".to_string(),
            JsonSchema::Integer {
                description: Some(
                    "Optional delay in milliseconds after completing the barrier".to_string(),
                ),
            },
        ),
        (
            "barrier".to_string(),
            JsonSchema::Object {
                properties: barrier_properties,
                required: Some(vec!["id".to_string(), "participants".to_string()]),
                additional_properties: Some(false.into()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "test_sync_tool".to_string(),
        description: "Internal synchronization helper used by Chaos integration tests.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_list_mcp_resources_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resources from every configured server."
                        .to_string(),
                ),
            },
        ),
        (
            "cursor".to_string(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resources call for the same server."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "list_mcp_resources".to_string(),
        description: "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_list_mcp_resource_templates_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resource templates from all configured servers."
                        .to_string(),
                ),
            },
        ),
        (
            "cursor".to_string(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resource_templates call for the same server."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "list_mcp_resource_templates".to_string(),
        description: "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_read_mcp_resource_tool() -> ToolSpec {
    let properties = mcp_resource_uri_properties(
        "Resource URI from list_mcp_resources, or tasks:// / tasks://get/<id> / tasks://result/<id>.",
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "read_mcp_resource".to_string(),
        description:
            "Read a specific resource from an MCP server given the server name and resource URI."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["server".to_string(), "uri".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_set_mcp_resource_subscription_tool() -> ToolSpec {
    let mut properties = mcp_resource_uri_properties(
        "Resource URI to subscribe to or unsubscribe from. The URI is passed to the server without requiring the resource to currently exist or appear in list_mcp_resources.",
    );
    properties.insert(
        "subscribed".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Set to true to subscribe to updates, or false to unsubscribe.".to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "set_mcp_resource_subscription".to_string(),
        description: "Subscribe to or unsubscribe from update notifications for an MCP resource. The server must advertise resource subscription support. Resources may be transient and do not need to currently exist or appear in resources/list.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "server".to_string(),
                "uri".to_string(),
                "subscribed".to_string(),
            ]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

fn mcp_resource_uri_properties(uri_description: &str) -> BTreeMap<String, JsonSchema> {
    BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources."
                        .to_string(),
                ),
            },
        ),
        (
            "uri".to_string(),
            JsonSchema::String {
                description: Some(uri_description.to_string()),
            },
        ),
    ])
}

pub(crate) fn create_call_mcp_tool_async_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some("MCP server name.".to_string()),
            },
        ),
        (
            "tool".to_string(),
            JsonSchema::String {
                description: Some("Tool name (must declare taskSupport).".to_string()),
            },
        ),
        (
            "arguments".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "ttl".to_string(),
            JsonSchema::Integer {
                description: Some("Task lifetime in milliseconds.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "call_mcp_tool_async".to_string(),
        description: "Invoke an MCP tool as an async task. Returns a task ID.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["server".to_string(), "tool".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

pub(crate) fn create_cancel_mcp_task_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some("MCP server name.".to_string()),
            },
        ),
        (
            "task_id".to_string(),
            JsonSchema::String {
                description: Some("Task ID to cancel.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "cancel_mcp_task".to_string(),
        description: "Cancel a running MCP task. Returns final task state.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["server".to_string(), "task_id".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}
