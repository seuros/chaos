use super::*;
use serde_json::json;

#[test]
fn metadata_links_the_static_view_and_marks_refresh_app_only() {
    let linked = linked_tool_ui().unwrap().into_meta();
    assert_eq!(linked["ui"]["resourceUri"], DASHBOARD_URI);
    assert!(!apps::is_app_only(&linked));
    let refresh = dashboard_tool_ui().unwrap().into_meta();
    assert_eq!(refresh["ui"]["visibility"], json!(["app"]));
    assert!(apps::is_app_only(&refresh));
    assert_eq!(DASHBOARD_TOOL, "workflow_dashboard");
    assert_eq!(CANCEL_TOOL, "cancel_workflow");
}

#[test]
fn listing_and_contents_share_deny_by_default_policy() {
    let policy = resource_policy().into_meta().unwrap();
    assert_eq!(
        policy["ui"]["csp"],
        json!({
            "connectDomains": [],
            "resourceDomains": [],
            "frameDomains": [],
            "baseUriDomains": [],
        })
    );
    assert_eq!(policy["ui"]["permissions"], json!({}));
    assert!(policy["ui"].get("domain").is_none());
    let listing = resource_router::<()>().unwrap().list();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].uri, DASHBOARD_URI);
    assert_eq!(listing[0].meta.as_ref(), Some(&policy));
    assert_eq!(listing[0].mime_type.as_deref(), Some(apps::MIME_TYPE));
    assert!(!listing[0].subscribable);
    let content = resource_content().unwrap().into_value();
    assert_eq!(content["text"], DASHBOARD_HTML);
    assert_eq!(content["_meta"], Value::Object(policy));
    assert_eq!(content["mimeType"], apps::MIME_TYPE);
}

#[test]
fn apps_gate_is_closed_without_negotiation() {
    let session = Session::new();
    assert!(!apps_supported(&VisibilityContext::new(&session)));
}

#[test]
fn static_page_excludes_active_markup_and_external_channels() {
    assert!(DASHBOARD_HTML.len() < 32 * 1024);
    for forbidden in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
        "EventSource",
        "localStorage",
        "sessionStorage",
        "<iframe",
        "<object",
        "<embed",
        "<img",
        "<form",
        "src=",
        "href=",
        "onclick=",
    ] {
        assert!(!DASHBOARD_HTML.contains(forbidden), "{forbidden}");
    }
    for directive in [
        "default-src 'none'",
        "connect-src 'none'",
        "object-src 'none'",
        "frame-src 'none'",
        "worker-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
    ] {
        assert!(DASHBOARD_HTML.contains(directive), "{directive}");
    }
    assert!(DASHBOARD_HTML.contains(".textContent ="));
    assert!(DASHBOARD_HTML.contains("<noscript>"));
    assert!(DASHBOARD_HTML.contains("workflow_status"));
    assert!(DASHBOARD_HTML.contains("cancel_workflow"));
}

#[test]
fn bridge_has_bounded_handshake_and_parent_checks() {
    for required in [
        "ui/initialize",
        "ui/notifications/initialized",
        "ui/notifications/tool-result",
        "ui/resource-teardown",
        "event.source !== window.parent",
        "event.origin !== hostOrigin",
        "appCapabilities: {}",
        "serverTools",
        "pending.size >= 2",
        "clearTimeout(call.timer)",
        "observer?.disconnect()",
        "MAX_TOTAL_STEPS = 512",
        "next_offset",
    ] {
        assert!(DASHBOARD_HTML.contains(required), "{required}");
    }
}

#[test]
fn text_fallback_reports_lifecycle_attempts_paging_and_omissions() {
    let payload = json!({
        "runs": [{
            "run_id": "own-run", "kind": "workflow", "status": {"state": "cancelling"},
            "truncated": true,
            "steps": [{
                "node_id": "ask", "attempt": 2,
                "status": {"state": "waiting_for_input"}, "truncated": true,
                "detail": "MUST_NOT_RENDER",
            }],
            "result": "MUST_NOT_RENDER",
        }],
        "next_offset": 10,
        "details_omitted": true,
    });
    let text = dashboard_text(&payload);
    for expected in [
        "own-run",
        "workflow",
        "cleanup pending",
        "ask attempt 2",
        "pending input",
        "[truncated]",
        "offset 10",
        "intentionally omitted",
        TEXT_FALLBACK,
    ] {
        assert!(text.contains(expected), "{expected}");
    }
    assert!(!text.contains("MUST_NOT_RENDER"));
    let terminal = json!({
        "run_id": "r", "kind": "synopsis", "status": {"state": "terminal", "outcome": "failure"},
        "steps": [],
    });
    assert!(dashboard_text(&terminal).contains("terminal: failure"));
    assert!(dashboard_text(&json!({ "runs": [] })).contains("No runs on this page"));
    assert!(dashboard_text(&Value::Null).contains(TEXT_FALLBACK));
}

#[test]
fn fallback_limits_work_and_utf8_output_without_rendering_payloads() {
    let step = json!({
        "node_id": "界".repeat(1000), "attempt": 1,
        "status": {"state": "terminal", "outcome": "failure"},
    });
    let run = json!({
        "run_id": "é".repeat(1000), "kind": "workflow",
        "status": {"state": "running"}, "steps": vec![step; 40],
    });
    let text = dashboard_text(&json!({ "runs": vec![run; 20] }));
    assert!(text.chars().count() < 16_000);
    assert!(text.len() < 48_000);
    assert!(text.contains("display truncated"));
    assert!(text.ends_with(TEXT_FALLBACK));
}
