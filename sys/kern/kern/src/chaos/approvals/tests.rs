use super::*;

#[test]
fn granular_policy_without_permission_requests_is_auto_denied() {
    let policy = ApprovalPolicy::Granular(crate::protocol::GranularApprovalConfig {
        sandbox_approval: true,
        rules: true,
        request_permissions: false,
        mcp_elicitations: true,
    });

    assert_eq!(
        automatic_request_permissions_response(policy),
        Some(RequestPermissionsResponse {
            permissions: RequestPermissionProfile::default(),
            scope: PermissionGrantScope::Turn,
        })
    );
}

#[test]
fn interactive_policy_requires_a_client_response() {
    assert_eq!(
        automatic_request_permissions_response(ApprovalPolicy::Interactive),
        None
    );
}
