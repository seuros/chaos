//! Kernel prompt rendering.

use crate::client_common::tools::ToolSpec;
use crate::config::Config;
use crate::modes::ModeCapabilities;
use crate::tools::groups::ToolGroupFilter;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::product::OS_NAME;
use chaos_ipc::protocol::SessionSource;
use include_dir::Dir;
use include_dir::include_dir;
use minijinja::AutoEscape;
use minijinja::Environment;
use minijinja::Error;
use minijinja::ErrorKind;
use minijinja::UndefinedBehavior;
use minijinja::Value;
use minijinja::syntax::SyntaxConfig;
use minijinja::value::Serde;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::LazyLock;

const TEMPLATE_NAME: &str = "prompt.md.j2";
const RUNTIME_TEMPLATE_NAME: &str = "runtime.md.j2";
static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates/prompt");
static COLLABORATION_TEMPLATES: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/templates/collaboration_mode");
static ENGINE: LazyLock<Environment<'static>> = LazyLock::new(template_environment);

#[derive(Default, Serialize)]
struct TemplateContext {
    model: ModelContext,
    project: ProjectContext,
    env: EnvironmentContext,
}

#[derive(Default, Serialize)]
struct ModelContext {
    id: Option<String>,
    family: Option<String>,
    provider_id: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Default, Serialize)]
struct ProjectContext {
    cwd: Option<String>,
}

#[derive(Serialize)]
struct EnvironmentContext {
    os: &'static str,
    os_family: &'static str,
    arch: &'static str,
}

/// The final request surface, after group, mode, and deferred-tool filtering.
#[derive(Serialize)]
struct RuntimeContext<'a> {
    tools: BTreeSet<&'a str>,
    disabled_groups: BTreeSet<String>,
    mode: ModeCapabilities,
    output: OutputContext,
}

#[derive(Serialize)]
struct OutputContext {
    terminal: bool,
}

impl<'a> RuntimeContext<'a> {
    fn new(
        tools: &'a [ToolSpec],
        tool_groups: ToolGroupFilter<'_>,
        mode: ModeCapabilities,
        source: &SessionSource,
        structured_output: bool,
        tui_output: bool,
    ) -> Self {
        let tools: BTreeSet<_> = tools.iter().map(ToolSpec::name).collect();
        let disabled_groups = if tools.contains("enable_tools") {
            tool_groups
                .catalog
                .disabled_groups(tool_groups.state)
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };
        Self {
            tools,
            disabled_groups,
            mode,
            output: OutputContext {
                terminal: cfg!(feature = "tui")
                    && tui_output
                    && matches!(source, SessionSource::Cli)
                    && !structured_output,
            },
        }
    }
}

impl Default for EnvironmentContext {
    fn default() -> Self {
        Self {
            os: std::env::consts::OS,
            os_family: std::env::consts::FAMILY,
            arch: std::env::consts::ARCH,
        }
    }
}

impl TemplateContext {
    fn new(model: &ModelInfo, config: &Config) -> Self {
        Self {
            model: ModelContext {
                id: Some(model.slug.clone()),
                family: Some(model.model_family.as_str().to_owned()),
                provider_id: Some(config.model_provider_id.clone()),
                reasoning_effort: config.model_reasoning_effort,
            },
            project: ProjectContext {
                cwd: Some(config.cwd.to_string_lossy().into_owned()),
            },
            env: EnvironmentContext::default(),
        }
    }
}

#[expect(clippy::expect_used, reason = "default delimiters are valid")]
fn template_environment() -> Environment<'static> {
    let mut engine = Environment::new();
    engine.add_global("OS_NAME", OS_NAME);
    engine.set_auto_escape_callback(|_| AutoEscape::None);
    engine.set_undefined_behavior(UndefinedBehavior::Strict);
    engine.set_syntax(
        SyntaxConfig::builder()
            .keep_trailing_newline(true)
            .trim_blocks(true)
            .lstrip_blocks(true)
            .build()
            .expect("default template syntax must be valid"),
    );
    engine.set_loader(|name| {
        let file = match name.strip_prefix("collaboration_mode/") {
            Some(name) => COLLABORATION_TEMPLATES.get_file(name),
            None => TEMPLATES.get_file(name),
        };
        let Some(file) = file else {
            return Ok(None);
        };
        let source = file.contents_utf8().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("bundled prompt template {name} is not UTF-8"),
            )
        })?;
        Ok(Some(source.to_owned()))
    });
    engine
}

#[expect(
    clippy::expect_used,
    reason = "bundled templates and the concrete context are validated by unit tests"
)]
pub(crate) fn render_template(name: &str, context: impl Into<Value>) -> String {
    ENGINE
        .get_template(name)
        .and_then(|template| template.render(context))
        .expect("bundled kernel prompt must render with the documented context")
}

fn render_context(context: &TemplateContext) -> String {
    render_template(TEMPLATE_NAME, Serde(context))
}

pub(crate) fn render(model: &ModelInfo, config: &Config) -> String {
    render_context(&TemplateContext::new(model, config))
}

/// Used when constructing a Prompt without model or session configuration.
pub(crate) fn default_instructions() -> String {
    render_context(&TemplateContext::default())
}

pub(crate) fn runtime_instructions(
    tools: &[ToolSpec],
    tool_groups: ToolGroupFilter<'_>,
    mode: ModeCapabilities,
    source: &SessionSource,
    structured_output: bool,
    tui_output: bool,
) -> String {
    render_template(
        RUNTIME_TEMPLATE_NAME,
        Serde(RuntimeContext::new(
            tools,
            tool_groups,
            mode,
            source,
            structured_output,
            tui_output,
        )),
    )
    .trim()
    .to_owned()
}

#[cfg(test)]
mod tests;
