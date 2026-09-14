use super::*;
use futures::future;

#[test]
fn initialize_advertises_chaos_fleet_experimental_capability() {
    let capabilities = client_capabilities();
    let experimental = capabilities.experimental.expect("experimental");
    assert!(experimental.contains_key(chaos_mcp_protocol::FLEET_EXPERIMENTAL_CAPABILITY));
}

#[test]
fn managed_client_info_reuses_the_supplied_identity() {
    let identity = McpClientIdentity::new();
    let first = managed_client_info(&identity);
    let second = managed_client_info(&identity);
    let first_id = first
        .description
        .as_deref()
        .and_then(|description| description.strip_prefix(MCP_CLIENT_ID_DESCRIPTION_PREFIX))
        .expect("managed client identity");
    let second_id = second
        .description
        .as_deref()
        .and_then(|description| description.strip_prefix(MCP_CLIENT_ID_DESCRIPTION_PREFIX))
        .expect("managed client identity");

    assert!(Uuid::parse_str(first_id).is_ok());
    assert_eq!(first_id, second_id);
}

#[test]
fn managed_client_identities_are_unique_when_created() {
    assert_ne!(McpClientIdentity::new(), McpClientIdentity::new());
}

#[test]
fn scoped_client_identities_are_stable_and_isolated() {
    let first = McpClientIdentity::for_scope("session-1", "review-service");
    let replay = McpClientIdentity::for_scope("session-1", "review-service");
    let other_server = McpClientIdentity::for_scope("session-1", "other");
    let other_session = McpClientIdentity::for_scope("session-2", "review-service");

    assert_eq!(first, replay);
    assert_ne!(first, other_server);
    assert_ne!(first, other_session);
}

#[test]
fn stdio_environment_uses_the_stable_client_identity_and_rejects_spoofing() {
    let identity = McpClientIdentity::new();
    let configured = HashMap::from([(
        CHAOS_MCP_CLIENT_ID_ENV.to_string(),
        "chaos:spoofed".to_string(),
    )]);

    let env = create_env_for_mcp_server(Some(configured), &[], &identity);

    assert_eq!(
        env.get(CHAOS_MCP_CLIENT_ID_ENV),
        Some(&identity.trusted_stdio_subject())
    );
}

fn sandbox_state(cwd: &str) -> SandboxState {
    SandboxState {
        vfs_policy: VfsPolicy::default(),
        socket_policy: SocketPolicy::default(),
        alcatraz_exe: PathBuf::from("/alcatraz"),
        sandbox_cwd: PathBuf::from(cwd),
    }
}

fn async_client(
    initial: SandboxState,
    startup_complete: bool,
    client: Result<ManagedClient, StartupOutcomeError>,
) -> AsyncManagedClient {
    AsyncManagedClient::for_tests(
        future::ready(client).boxed().shared(),
        None,
        Arc::new(AtomicBool::new(startup_complete)),
        Arc::new(StdRwLock::new(initial.sandbox_cwd)),
    )
}

#[tokio::test]
async fn sandbox_update_is_retained_while_server_is_starting() {
    let initial = sandbox_state("/initial");
    let client = async_client(initial, false, Err(StartupOutcomeError::Cancelled));
    let updated = sandbox_state("/updated");

    client
        .notify_sandbox_state_change(&updated)
        .await
        .expect("queue sandbox state");

    assert_eq!(
        client
            .sandbox_state
            .read()
            .expect("MCP sandbox state lock poisoned")
            .sandbox_cwd,
        PathBuf::from("/updated")
    );
}

#[tokio::test]
async fn sandbox_update_ignores_already_reported_startup_failure() {
    let initial = sandbox_state("/initial");
    let client = async_client(initial, true, Err(StartupOutcomeError::Cancelled));

    client
        .notify_sandbox_state_change(&sandbox_state("/updated"))
        .await
        .expect("failed optional server should not reject permission updates");
}
