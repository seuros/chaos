#[cfg(feature = "stdio")]
use std::collections::HashMap;
#[cfg(feature = "stdio")]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::GuestError;
use crate::handler::ClientHandler;
use crate::handler::NoopClientHandler;
use crate::protocol::ClientCapabilities;
use crate::protocol::ElicitationCapability;
use crate::protocol::EmptyObject;
use crate::protocol::FormElicitationCapability;
use crate::protocol::Implementation;
use crate::protocol::RootsCapability;
use crate::protocol::SamplingCapability;
use crate::protocol::TasksCapability;
use crate::protocol::UrlElicitationCapability;
use crate::runtime::ConnectionOptions;
use crate::runtime::connect_with_transport;
use crate::session::McpSession;
#[cfg(feature = "http")]
use crate::transport::http::HttpClientConfig;
#[cfg(feature = "http")]
use crate::transport::http::HttpTransport;
#[cfg(feature = "stdio")]
use crate::transport::stdio::StdioChild;
#[cfg(feature = "stdio")]
use crate::transport::stdio::StdioProcessConfig;
#[cfg(feature = "stdio")]
use crate::transport::stdio::StdioTransport;
use chaos_ipc::product::CHAOS_VERSION;
#[cfg(feature = "http")]
use url::Url;

#[cfg(feature = "stdio")]
pub fn stdio(command: &str, args: &[String]) -> StdioBuilder {
    StdioBuilder {
        process: StdioProcessConfig {
            command: command.to_string(),
            args: args.to_vec(),
            env: HashMap::new(),
            cwd: None,
            shutdown_timeout: Duration::from_secs(5),
            kill_timeout: Duration::from_secs(5),
        },
        client_info: Implementation::new("mcp-guest", CHAOS_VERSION),
        capabilities: ClientCapabilities::default(),
        handler: Arc::new(NoopClientHandler),
        default_timeout: Duration::from_secs(30),
    }
}

#[cfg(feature = "http")]
pub fn http(endpoint: &str) -> HttpBuilder {
    HttpBuilder {
        endpoint: endpoint.to_string(),
        open_sse_stream: true,
        reconnect_delay: Duration::from_millis(500),
        default_headers: Vec::new(),
        client_info: Implementation::new("mcp-guest", CHAOS_VERSION),
        capabilities: ClientCapabilities::default(),
        handler: Arc::new(NoopClientHandler),
        default_timeout: Duration::from_secs(30),
    }
}

#[cfg(feature = "stdio")]
pub struct StdioBuilder {
    process: StdioProcessConfig,
    client_info: Implementation,
    capabilities: ClientCapabilities,
    handler: Arc<dyn ClientHandler>,
    default_timeout: Duration,
}

#[cfg(feature = "http")]
pub struct HttpBuilder {
    endpoint: String,
    open_sse_stream: bool,
    reconnect_delay: Duration,
    default_headers: Vec<(String, String)>,
    client_info: Implementation,
    capabilities: ClientCapabilities,
    handler: Arc<dyn ClientHandler>,
    default_timeout: Duration,
}

#[cfg(feature = "stdio")]
impl StdioBuilder {
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.process.env.insert(key.to_string(), value.to_string());
        self
    }

    pub fn envs(mut self, env: &HashMap<String, String>) -> Self {
        self.process
            .env
            .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.process.cwd = Some(cwd.into());
        self
    }

    pub fn client_info(mut self, client_info: Implementation) -> Self {
        self.client_info = client_info;
        self
    }

    pub fn client_name(mut self, name: &str) -> Self {
        self.client_info.name = name.to_string();
        self
    }

    pub fn client_version(mut self, version: &str) -> Self {
        self.client_info.version = version.to_string();
        self
    }

    pub fn capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn enable_roots(mut self, list_changed: bool) -> Self {
        self.capabilities.roots = Some(RootsCapability {
            list_changed: Some(list_changed),
        });
        self
    }

    pub fn enable_sampling(mut self) -> Self {
        self.capabilities.sampling = Some(SamplingCapability {
            context: Some(EmptyObject::default()),
            tools: Some(EmptyObject::default()),
        });
        self
    }

    pub fn enable_form_elicitation(mut self) -> Self {
        self.capabilities.elicitation = Some(ElicitationCapability {
            form: Some(FormElicitationCapability {}),
            url: None,
        });
        self
    }

    pub fn enable_url_elicitation(mut self) -> Self {
        let mut capability = self.capabilities.elicitation.take().unwrap_or_default();
        capability.url = Some(UrlElicitationCapability {});
        self.capabilities.elicitation = Some(capability);
        self
    }

    pub fn enable_tasks(mut self, tasks: TasksCapability) -> Self {
        self.capabilities.tasks = Some(tasks);
        self
    }

    pub fn handler<H>(mut self, handler: H) -> Self
    where
        H: ClientHandler,
    {
        self.handler = Arc::new(handler);
        self
    }

    pub fn handler_arc(mut self, handler: Arc<dyn ClientHandler>) -> Self {
        self.handler = handler;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.process.shutdown_timeout = timeout;
        self
    }

    pub fn kill_timeout(mut self, timeout: Duration) -> Self {
        self.process.kill_timeout = timeout;
        self
    }

    pub async fn connect(self) -> Result<McpSession, GuestError> {
        let child = StdioChild::spawn(
            &self.process.command,
            &self.process.args,
            &self.process.env,
            self.process.cwd.as_deref(),
        )?;
        let transport = StdioTransport::new(
            child,
            self.process.shutdown_timeout,
            self.process.kill_timeout,
        );

        connect_with_transport(
            transport,
            ConnectionOptions {
                client_info: self.client_info,
                capabilities: self.capabilities,
                handler: self.handler,
                default_timeout: self.default_timeout,
            },
        )
        .await
    }
}

#[cfg(feature = "http")]
impl HttpBuilder {
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_headers.push((name.into(), value.into()));
        self
    }

    pub fn headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.default_headers
            .extend(headers.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.default_headers.push((
            "Authorization".to_string(),
            format!("Bearer {}", token.into()),
        ));
        self
    }

    pub fn client_info(mut self, client_info: Implementation) -> Self {
        self.client_info = client_info;
        self
    }

    pub fn client_name(mut self, name: &str) -> Self {
        self.client_info.name = name.to_string();
        self
    }

    pub fn client_version(mut self, version: &str) -> Self {
        self.client_info.version = version.to_string();
        self
    }

    pub fn capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn enable_roots(mut self, list_changed: bool) -> Self {
        self.capabilities.roots = Some(RootsCapability {
            list_changed: Some(list_changed),
        });
        self
    }

    pub fn enable_sampling(mut self) -> Self {
        self.capabilities.sampling = Some(SamplingCapability {
            context: Some(EmptyObject::default()),
            tools: Some(EmptyObject::default()),
        });
        self
    }

    pub fn enable_form_elicitation(mut self) -> Self {
        self.capabilities.elicitation = Some(ElicitationCapability {
            form: Some(FormElicitationCapability {}),
            url: None,
        });
        self
    }

    pub fn enable_url_elicitation(mut self) -> Self {
        let mut capability = self.capabilities.elicitation.take().unwrap_or_default();
        capability.url = Some(UrlElicitationCapability {});
        self.capabilities.elicitation = Some(capability);
        self
    }

    pub fn enable_tasks(mut self, tasks: TasksCapability) -> Self {
        self.capabilities.tasks = Some(tasks);
        self
    }

    pub fn enable_sse_stream(mut self, enabled: bool) -> Self {
        self.open_sse_stream = enabled;
        self
    }

    pub fn reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    pub fn handler<H>(mut self, handler: H) -> Self
    where
        H: ClientHandler,
    {
        self.handler = Arc::new(handler);
        self
    }

    pub fn handler_arc(mut self, handler: Arc<dyn ClientHandler>) -> Self {
        self.handler = handler;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub async fn connect(self) -> Result<McpSession, GuestError> {
        let endpoint =
            Url::parse(&self.endpoint).map_err(|error| GuestError::UrlParse(error.to_string()))?;
        match endpoint.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(GuestError::UrlParse(format!(
                    "unsupported http transport scheme: {scheme}"
                )));
            }
        }

        let transport = HttpTransport::new(HttpClientConfig {
            endpoint,
            open_sse_stream: self.open_sse_stream,
            reconnect_delay: self.reconnect_delay,
            default_headers: self.default_headers,
        });

        connect_with_transport(
            transport,
            ConnectionOptions {
                client_info: self.client_info,
                capabilities: self.capabilities,
                handler: self.handler,
                default_timeout: self.default_timeout,
            },
        )
        .await
    }
}
