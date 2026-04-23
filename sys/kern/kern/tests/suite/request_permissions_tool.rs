#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(target_os = "macos")]

use crate::request_permissions_test_support::build_add_file_patch;
use crate::request_permissions_test_support::exec_command_event;
use crate::request_permissions_test_support::expect_request_permissions_event;
use crate::request_permissions_test_support::normalized_directory_write_permissions;
use crate::request_permissions_test_support::parse_result;
use crate::request_permissions_test_support::request_permissions_tool_event;
use crate::request_permissions_test_support::requested_directory_write_permissions;
use crate::request_permissions_test_support::submit_turn;
use crate::request_permissions_test_support::workspace_write_excluding_tmp;
use anyhow::Result;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_kern::config::Constrained;
use chaos_kern::features::Feature;
use core_test_support::responses::ev_apply_patch_function_call;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;

#[tokio::test(flavor = "current_thread")]
#[cfg(target_os = "macos")]
async fn approved_folder_write_request_permissions_unblocks_later_apply_patch_without_prompt()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let approval_policy = ApprovalPolicy::Interactive;
    let sandbox_policy = workspace_write_excluding_tmp();
    let sandbox_policy_for_config = sandbox_policy.clone();

    let mut builder = test_chaos().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.permissions.sandbox_policy = Constrained::allow_any(sandbox_policy_for_config);
        config
            .features
            .enable(Feature::ExecPermissionApprovals)
            .expect("test config should allow feature update");
        config
            .features
            .enable(Feature::RequestPermissionsTool)
            .expect("test config should allow feature update");
    });
    let test = builder.build(&server).await?;

    let requested_dir = tempfile::tempdir()?;
    let requested_file = requested_dir.path().join("allowed-patch.txt");
    let requested_permissions = requested_directory_write_permissions(requested_dir.path());
    let normalized_requested_permissions =
        normalized_directory_write_permissions(requested_dir.path())?;
    let patch = build_add_file_patch(&requested_file, "patched-via-request-permissions");

    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-request-permissions-patch-1"),
                request_permissions_tool_event(
                    "permissions-call",
                    "Allow patching outside the workspace",
                    &requested_permissions,
                )?,
                ev_completed("resp-request-permissions-patch-1"),
            ]),
            sse(vec![
                ev_response_created("resp-request-permissions-patch-2"),
                ev_apply_patch_function_call("apply-patch-call", &patch),
                ev_completed("resp-request-permissions-patch-2"),
            ]),
            sse(vec![
                ev_response_created("resp-request-permissions-patch-3"),
                ev_assistant_message("msg-request-permissions-patch-1", "done"),
                ev_completed("resp-request-permissions-patch-3"),
            ]),
        ],
    )
    .await;

    submit_turn(
        &test,
        "patch outside the workspace",
        approval_policy,
        sandbox_policy,
    )
    .await?;

    let granted_permissions = expect_request_permissions_event(&test, "permissions-call").await;
    assert_eq!(
        granted_permissions,
        normalized_requested_permissions.clone()
    );
    test.process
        .submit(Op::RequestPermissionsResponse {
            id: "permissions-call".to_string(),
            response: RequestPermissionsResponse {
                permissions: normalized_requested_permissions,
                scope: PermissionGrantScope::Turn,
            },
        })
        .await?;

    let event = wait_for_event(&test.process, |event| {
        matches!(
            event,
            EventMsg::ApplyPatchApprovalRequest(_) | EventMsg::TurnComplete(_)
        )
    })
    .await;
    match event {
        EventMsg::TurnComplete(_) => {}
        EventMsg::ApplyPatchApprovalRequest(approval) => {
            panic!(
                "unexpected apply_patch approval request after granted permissions: {:?}",
                approval.call_id
            )
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let patch_output = responses
        .function_call_output_text("apply-patch-call")
        .map(|output| json!({ "output": output }))
        .unwrap_or_else(|| panic!("expected apply-patch-call output"));
    let result = parse_result(&patch_output);
    assert!(result.exit_code.is_none() || result.exit_code == Some(0));
    assert!(
        !result.stdout.trim().is_empty(),
        "expected apply_patch to report some output"
    );
    assert_eq!(
        fs::read_to_string(&requested_file)?,
        "patched-via-request-permissions\n"
    );

    Ok(())
}
