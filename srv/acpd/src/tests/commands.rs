use super::*;
use chaos_kern::test_support::test_model_info;
use chaos_kern::user_documents::{self, DocumentKind};

fn advertised(updates: &[Value]) -> Vec<String> {
    updates
        .iter()
        .filter_map(|frame| frame["params"]["update"]["availableCommands"].as_array())
        .flatten()
        .map(|command| command["name"].as_str().unwrap().to_owned())
        .collect()
}

pub(super) fn assert_only_commands(updates: &[Value]) {
    assert!(!updates.is_empty());
    assert!(
        updates
            .iter()
            .all(|frame| frame["params"]["update"]["sessionUpdate"] == "available_commands_update")
    );
}

async fn run(peer: &mut Peer, session: &Value, text: &str) -> Vec<Value> {
    let id = peer.prompt(session, text).await;
    peer.finish(session, id, "end_turn").await
}

fn text(updates: &[Value]) -> String {
    updates
        .iter()
        .filter_map(|frame| frame["params"]["update"]["content"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_are_advertised_and_local_commands_never_reach_the_model() {
    let mut fixture = Fixture::new().await;
    fixture.config.model_catalog = Some(
        serde_json::from_value(json!({
            "models":[test_model_info("mock-model"),test_model_info("mock-second")]
        }))
        .unwrap(),
    );
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![response("after-command", "real turn")],
    )
    .await;
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (opened, updates) = peer
        .request(
            "session/new",
            json!({
                "cwd":fixture.cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    let session = opened["result"]["sessionId"].clone();
    let names = advertised(&updates);
    for name in [
        "help", "status", "compact", "review", "rename", "model", "mode", "plan",
    ] {
        assert!(names.iter().any(|entry| entry == name), "{names:?}");
    }
    for name in ["permissions", "quit", "theme", "accounts"] {
        assert!(!names.iter().any(|entry| entry == name));
    }
    assert!(text(&run(&mut peer, &session, "/status").await).contains("mock-model"));
    assert!(text(&run(&mut peer, &session, "/help").await).contains("/compact"));
    assert!(text(&run(&mut peer, &session, "/model mock-second").await).contains("Selected model"));
    assert!(
        text(&run(&mut peer, &session, "/model forbidden-provider/model").await)
            .contains("Cannot select")
    );
    assert!(text(&run(&mut peer, &session, "/mode plan").await).contains("Selected mode"));
    assert!(text(&run(&mut peer, &session, "/status").await).contains("Mode: plan"));
    assert!(text(&run(&mut peer, &session, "/mode default").await).contains("Selected mode"));
    assert!(
        text(&run(&mut peer, &session, "/rename ACP command test").await)
            .contains("Session renamed")
    );
    for input in [
        "/quit",
        "/unknown",
        "/compact unexpected",
        "/prompts:unknown",
    ] {
        let updates = run(&mut peer, &session, input).await;
        assert!(!text(&updates).is_empty(), "{input}");
    }
    assert!(
        requests.requests().is_empty(),
        "local commands must not invoke a provider"
    );
    run(&mut peer, &session, "ordinary user prompt").await;
    assert_eq!(requests.requests().len(), 1);
    assert_eq!(requests.requests()[0].body_json()["model"], "mock-second");
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_prompt_commands_share_console_expansion_and_refresh_enabled_state() {
    let fixture = Fixture::new().await;
    let home = &fixture.config.chaos_home;
    user_documents::import(home, DocumentKind::Prompt, "named",
        "---\ndescription: Review a file\nargument-hint: FILE=\"path\"\n---\nReview $FILE carefully"
    ).await.unwrap();
    user_documents::import(
        home,
        DocumentKind::Prompt,
        "positional",
        "Inspect $1 and $ARGUMENTS",
    )
    .await
    .unwrap();
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            response("named", "named expansion"),
            response("positional", "positional expansion"),
            response("plan", "plan task"),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    let updates = run(&mut peer, &session, "/help").await;
    assert!(advertised(&updates).contains(&"prompts:named".to_string()));
    run(
        &mut peer,
        &session,
        "/prompts:named FILE=\"src/my file.rs\"",
    )
    .await;
    let body = requests.requests()[0].body_json().to_string();
    assert!(body.contains("Review src/my file.rs carefully"), "{body}");
    assert!(!body.contains("/prompts:named"));
    run(&mut peer, &session, "/prompts:positional \"a b\" c").await;
    assert!(
        requests.requests()[1]
            .body_json()
            .to_string()
            .contains("Inspect a b and a b c")
    );
    let missing = run(&mut peer, &session, "/prompts:named").await;
    assert!(text(&missing).contains("Missing required args"));
    user_documents::set_enabled(home, DocumentKind::Prompt, "named", false)
        .await
        .unwrap();
    let disabled = run(&mut peer, &session, "/prompts:named FILE=ignored").await;
    assert!(!advertised(&disabled).contains(&"prompts:named".to_string()));
    assert!(text(&disabled).contains("Unknown or disabled"));
    assert_eq!(requests.requests().len(), 2);
    run(&mut peer, &session, "/plan investigate the code").await;
    let body = requests.requests()[2].body_json().to_string();
    assert!(body.contains("investigate the code"));
    assert!(!body.contains("/plan investigate"));
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_mode_selection_cannot_raise_a_fixed_native_mode_ceiling() {
    let mut fixture = Fixture::new().await;
    chaos_kern::test_support::set_mode_policy_for_test(
        &mut fixture.config,
        "plan",
        &["plan"],
        false,
    );
    let (mut peer, session) = fixture.peer().await;
    let updates = run(&mut peer, &session, "/mode default").await;
    assert!(text(&updates).contains("Cannot select"));
    assert!(text(&run(&mut peer, &session, "/status").await).contains("Mode: plan"));
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_and_review_dispatch_native_operations_not_literal_prompts() {
    let fixture = Fixture::new().await;
    let review = json!({
        "findings":[], "overall_correctness":"patch is correct",
        "overall_explanation":"native review complete", "overall_confidence_score":0.9
    })
    .to_string();
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            response("initial", "initial conversation"),
            response("compact", "a native compaction summary"),
            response("review", &review),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    run(&mut peer, &session, "seed the conversation").await;
    run(&mut peer, &session, "/compact").await;
    run(&mut peer, &session, "/review inspect validation carefully").await;
    let calls = requests.requests();
    assert_eq!(calls.len(), 3);
    let compact = calls[1].body_json().to_string();
    assert!(!compact.contains("/compact"));
    assert!(compact.contains("summary") || compact.contains("summar"));
    let review = calls[2].body_json().to_string();
    assert!(review.contains("inspect validation carefully"));
    assert!(!review.contains("/review inspect"));
    peer.close().await;
}
