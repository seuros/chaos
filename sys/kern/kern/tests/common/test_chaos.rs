use std::mem::swap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::user_input::UserInput;
use chaos_kern::ChaosAuth;
use chaos_kern::ModelProviderInfo;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::built_in_model_providers;
use chaos_kern::config::Config;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::MockServer;

use crate::load_default_config_for_test;
use crate::responses::output_value_to_text;
use crate::responses::start_mock_server;
use crate::streaming_sse::StreamingSseServer;
use crate::wait_for_event;
use crate::wait_for_event_match;
use wiremock::Match;
use wiremock::matchers::path_regex;

type ConfigMutator = dyn FnOnce(&mut Config) + Send;
type PreBuildHook = dyn FnOnce(&Path) + Send + 'static;
const TEST_MODEL_WITH_EXPERIMENTAL_TOOLS: &str = "test-serpent";

/// A collection of different ways the model can output an apply_patch call
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApplyPatchModelOutput {
    Freeform,
    Function,
    Shell,
    ShellViaHeredoc,
    ShellCommandViaHeredoc,
}

/// A collection of different ways the model can output an apply_patch call
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShellModelOutput {
    Shell,
    ShellCommand,
    LocalShell,
    // UnifiedExec has its own set of tests
}

pub struct TestCodexBuilder {
    config_mutators: Vec<Box<ConfigMutator>>,
    auth: ChaosAuth,
    pre_build_hooks: Vec<Box<PreBuildHook>>,
    home: Option<Arc<TempDir>>,
}

impl TestCodexBuilder {
    pub fn with_config<T>(mut self, mutator: T) -> Self
    where
        T: FnOnce(&mut Config) + Send + 'static,
    {
        self.config_mutators.push(Box::new(mutator));
        self
    }

    pub fn with_auth(mut self, auth: ChaosAuth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_model(self, model: &str) -> Self {
        let new_model = model.to_string();
        self.with_config(move |config| {
            config.model = Some(new_model.clone());
        })
    }

    pub fn with_pre_build_hook<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(&Path) + Send + 'static,
    {
        self.pre_build_hooks.push(Box::new(hook));
        self
    }

    pub fn with_home(mut self, home: Arc<TempDir>) -> Self {
        self.home = Some(home);
        self
    }

    pub async fn build(&mut self, server: &wiremock::MockServer) -> anyhow::Result<TestCodex> {
        let home = match self.home.clone() {
            Some(home) => home,
            None => Arc::new(TempDir::new()?),
        };
        Box::pin(self.build_with_home(server, home, /*resume_from*/ None)).await
    }

    pub async fn build_with_streaming_server(
        &mut self,
        server: &StreamingSseServer,
    ) -> anyhow::Result<TestCodex> {
        let base_url = server.uri();
        let home = match self.home.clone() {
            Some(home) => home,
            None => Arc::new(TempDir::new()?),
        };
        Box::pin(self.build_with_home_and_base_url(
            format!("{base_url}/v1"),
            home,
            /*resume_from*/ None,
        ))
        .await
    }

    pub async fn resume(
        &mut self,
        server: &wiremock::MockServer,
        home: Arc<TempDir>,
        process_id: ProcessId,
    ) -> anyhow::Result<TestCodex> {
        Box::pin(self.build_with_home(server, home, Some(process_id))).await
    }

    async fn build_with_home(
        &mut self,
        server: &wiremock::MockServer,
        home: Arc<TempDir>,
        resume_from: Option<ProcessId>,
    ) -> anyhow::Result<TestCodex> {
        let base_url = format!("{}/v1", server.uri());
        let (config, cwd) = self.prepare_config(base_url, &home).await?;
        Box::pin(self.build_from_config(config, cwd, home, resume_from)).await
    }

    async fn build_with_home_and_base_url(
        &mut self,
        base_url: String,
        home: Arc<TempDir>,
        resume_from: Option<ProcessId>,
    ) -> anyhow::Result<TestCodex> {
        let (config, cwd) = self.prepare_config(base_url, &home).await?;
        Box::pin(self.build_from_config(config, cwd, home, resume_from)).await
    }

    async fn build_from_config(
        &mut self,
        config: Config,
        cwd: Arc<TempDir>,
        home: Arc<TempDir>,
        resume_from: Option<ProcessId>,
    ) -> anyhow::Result<TestCodex> {
        let auth = self.auth.clone();
        let process_table = if config.model_catalog.is_some() {
            ProcessTable::new(
                &config,
                chaos_kern::test_support::auth_manager_from_auth(auth.clone()),
                SessionSource::Exec,
                CollaborationModesConfig::default(),
            )
        } else {
            chaos_kern::test_support::process_table_with_models_provider_and_home(
                auth.clone(),
                config.model_provider.clone(),
                config.chaos_home.clone(),
            )
        };
        let process_table = Arc::new(process_table);

        let new_conversation = match resume_from {
            Some(process_id) => {
                let auth_manager = chaos_kern::test_support::auth_manager_from_auth(auth);
                Box::pin(process_table.resume_process(
                    config.clone(),
                    process_id,
                    auth_manager,
                    /*parent_trace*/ None,
                ))
                .await?
            }
            None => Box::pin(process_table.start_process(config.clone())).await?,
        };

        Ok(TestCodex {
            home,
            cwd,
            config,
            process: new_conversation.process,
            session_configured: new_conversation.session_configured,
            process_table,
        })
    }

    async fn prepare_config(
        &mut self,
        base_url: String,
        home: &TempDir,
    ) -> anyhow::Result<(Config, Arc<TempDir>)> {
        let model_provider = ModelProviderInfo {
            base_url: Some(base_url),
            ..built_in_model_providers(/*openai_base_url*/ None)["openai"].clone()
        };
        let cwd = Arc::new(TempDir::new()?);
        let mut config = load_default_config_for_test(home).await;
        config.cwd = cwd.path().to_path_buf();
        config.model_provider = model_provider;
        // Prevent real user scripts (~/.config/chaos/scripts/) from loading
        // into test sessions and polluting the tool list.
        config.disable_user_scripts = true;
        for hook in self.pre_build_hooks.drain(..) {
            hook(home.path());
        }
        if let Ok(path) = chaos_which::cargo_bin("chaos") {
            config.alcatraz_linux_exe = Some(path);
        } else if let Ok(exe) = std::env::current_exe()
            && let Some(path) = exe
                .parent()
                .and_then(|parent| parent.parent())
                .map(|parent| parent.join("chaos"))
            && path.is_file()
        {
            config.alcatraz_linux_exe = Some(path);
        }

        let mut mutators = vec![];
        swap(&mut self.config_mutators, &mut mutators);
        for mutator in mutators {
            mutator(&mut config);
        }
        ensure_test_model_catalog(&mut config)?;

        Ok((config, cwd))
    }
}

fn ensure_test_model_catalog(config: &mut Config) -> Result<()> {
    if config.model.as_deref() != Some(TEST_MODEL_WITH_EXPERIMENTAL_TOOLS)
        || config.model_catalog.is_some()
    {
        return Ok(());
    }

    let mut model = chaos_kern::test_support::test_model_info(TEST_MODEL_WITH_EXPERIMENTAL_TOOLS);
    model.experimental_supported_tools = vec![
        "test_sync_tool".to_string(),
        "read_file".to_string(),
        "grep_files".to_string(),
        "list_dir".to_string(),
    ];
    config.model_catalog = Some(chaos_ipc::openai_models::ModelsResponse {
        models: vec![model],
    });
    Ok(())
}

pub struct TestCodex {
    pub home: Arc<TempDir>,
    pub cwd: Arc<TempDir>,
    pub process: Arc<Process>,
    pub session_configured: SessionConfiguredEvent,
    pub config: Config,
    pub process_table: Arc<ProcessTable>,
}

impl TestCodex {
    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }

    pub fn chaos_home_path(&self) -> &Path {
        self.config.chaos_home.as_path()
    }

    pub fn workspace_path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.cwd_path().join(rel)
    }

    pub async fn submit_turn(&self, prompt: &str) -> Result<()> {
        self.submit_turn_with_policies(prompt, ApprovalPolicy::Headless, SandboxPolicy::RootAccess)
            .await
    }

    pub async fn submit_turn_with_policy(
        &self,
        prompt: &str,
        sandbox_policy: SandboxPolicy,
    ) -> Result<()> {
        self.submit_turn_with_policies(prompt, ApprovalPolicy::Headless, sandbox_policy)
            .await
    }

    pub async fn submit_turn_with_service_tier(
        &self,
        prompt: &str,
        service_tier: Option<ServiceTier>,
    ) -> Result<()> {
        self.submit_turn_with_context(
            prompt,
            ApprovalPolicy::Headless,
            SandboxPolicy::RootAccess,
            Some(service_tier),
        )
        .await
    }

    pub async fn submit_turn_with_policies(
        &self,
        prompt: &str,
        approval_policy: ApprovalPolicy,
        sandbox_policy: SandboxPolicy,
    ) -> Result<()> {
        self.submit_turn_with_context(
            prompt,
            approval_policy,
            sandbox_policy,
            /*service_tier*/ None,
        )
        .await
    }

    async fn submit_turn_with_context(
        &self,
        prompt: &str,
        approval_policy: ApprovalPolicy,
        sandbox_policy: SandboxPolicy,
        service_tier: Option<Option<ServiceTier>>,
    ) -> Result<()> {
        let session_model = self.session_configured.model.clone();
        self.process
            .submit(Op::UserTurn {
                items: vec![UserInput::Text {
                    text: prompt.into(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
                cwd: self.cwd.path().to_path_buf(),
                approval_policy,
                sandbox_policy,
                model: session_model,
                effort: None,
                summary: None,
                service_tier,
                collaboration_mode: None,
                personality: None,
            })
            .await?;

        let turn_id = wait_for_event_match(&self.process, |event| match event {
            EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
            _ => None,
        })
        .await;
        wait_for_event(&self.process, |event| match event {
            EventMsg::TurnComplete(event) => event.turn_id == turn_id,
            _ => false,
        })
        .await;
        Ok(())
    }
}

pub struct TestCodexHarness {
    server: MockServer,
    test: TestCodex,
}

impl TestCodexHarness {
    pub async fn new() -> Result<Self> {
        Self::with_builder(test_chaos()).await
    }

    pub async fn with_config(mutator: impl FnOnce(&mut Config) + Send + 'static) -> Result<Self> {
        Self::with_builder(test_chaos().with_config(mutator)).await
    }

    pub async fn with_builder(mut builder: TestCodexBuilder) -> Result<Self> {
        let server = start_mock_server().await;
        let test = builder.build(&server).await?;
        Ok(Self { server, test })
    }

    pub fn server(&self) -> &MockServer {
        &self.server
    }

    pub fn test(&self) -> &TestCodex {
        &self.test
    }

    pub fn cwd(&self) -> &Path {
        self.test.cwd_path()
    }

    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.test.workspace_path(rel)
    }

    pub async fn submit(&self, prompt: &str) -> Result<()> {
        self.test.submit_turn(prompt).await
    }

    pub async fn submit_with_policy(
        &self,
        prompt: &str,
        sandbox_policy: SandboxPolicy,
    ) -> Result<()> {
        self.test
            .submit_turn_with_policy(prompt, sandbox_policy)
            .await
    }

    pub async fn request_bodies(&self) -> Vec<Value> {
        let path_matcher = path_regex(".*/responses$");
        self.server
            .received_requests()
            .await
            .expect("mock server should not fail")
            .into_iter()
            .filter(|req| path_matcher.matches(req))
            .map(|req| {
                req.body_json::<Value>()
                    .expect("request body to be valid JSON")
            })
            .collect()
    }

    pub async fn function_call_output_value(&self, call_id: &str) -> Value {
        let bodies = self.request_bodies().await;
        function_call_output(&bodies, call_id).clone()
    }

    pub async fn function_call_stdout(&self, call_id: &str) -> String {
        self.function_call_output_value(call_id)
            .await
            .get("output")
            .and_then(Value::as_str)
            .expect("output string")
            .to_string()
    }

    pub async fn custom_tool_call_output(&self, call_id: &str) -> String {
        let bodies = self.request_bodies().await;
        custom_tool_call_output_text(&bodies, call_id)
    }

    pub async fn apply_patch_output(
        &self,
        call_id: &str,
        output_type: ApplyPatchModelOutput,
    ) -> String {
        match output_type {
            ApplyPatchModelOutput::Freeform => self.custom_tool_call_output(call_id).await,
            ApplyPatchModelOutput::Function
            | ApplyPatchModelOutput::Shell
            | ApplyPatchModelOutput::ShellViaHeredoc
            | ApplyPatchModelOutput::ShellCommandViaHeredoc => {
                self.function_call_stdout(call_id).await
            }
        }
    }
}

fn custom_tool_call_output<'a>(bodies: &'a [Value], call_id: &str) -> &'a Value {
    for body in bodies {
        if let Some(items) = body.get("input").and_then(Value::as_array) {
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("custom_tool_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some(call_id)
                {
                    return item;
                }
            }
        }
    }
    panic!("custom_tool_call_output {call_id} not found");
}

fn custom_tool_call_output_text(bodies: &[Value], call_id: &str) -> String {
    let output = custom_tool_call_output(bodies, call_id)
        .get("output")
        .unwrap_or_else(|| panic!("custom_tool_call_output {call_id} missing output"));
    output_value_to_text(output)
        .unwrap_or_else(|| panic!("custom_tool_call_output {call_id} missing text output"))
}

fn function_call_output<'a>(bodies: &'a [Value], call_id: &str) -> &'a Value {
    for body in bodies {
        if let Some(items) = body.get("input").and_then(Value::as_array) {
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some(call_id)
                {
                    return item;
                }
            }
        }
    }
    panic!("function_call_output {call_id} not found");
}

pub fn test_chaos() -> TestCodexBuilder {
    TestCodexBuilder {
        config_mutators: vec![],
        auth: ChaosAuth::from_api_key("dummy"),
        pre_build_hooks: vec![],
        home: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn custom_tool_call_output_text_returns_output_text() {
        let bodies = vec![json!({
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "hello"
            }]
        })];

        assert_eq!(custom_tool_call_output_text(&bodies, "call-1"), "hello");
    }

    #[test]
    #[should_panic(expected = "custom_tool_call_output call-2 missing output")]
    fn custom_tool_call_output_text_panics_when_output_is_missing() {
        let bodies = vec![json!({
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-2"
            }]
        })];

        let _ = custom_tool_call_output_text(&bodies, "call-2");
    }
}
