use std::collections::BTreeMap;
use std::error::Error;

use chaos_synopsis::tree::{
    Binding, BindingError, Domain, Predicate, TreeErrorCode, TreeNode, ValidatedTree,
    is_control_tool, resolve_arguments,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

fn call(id: impl Into<String>) -> TreeNode {
    TreeNode::Call {
        id: id.into(),
        tool: "read_file".into(),
        arguments: json!({}),
        bindings: Vec::new(),
        result_key: None,
    }
}

fn tools(root: TreeNode) -> Result<ValidatedTree, chaos_synopsis::tree::TreeError> {
    ValidatedTree::new(
        root,
        Domain::Tools {
            is_idempotent: &|_| true,
        },
    )
}

fn invalid(root: TreeNode, code: TreeErrorCode) {
    std::assert_matches!(tools(root), Err(error) if error.code == code);
}

#[test]
fn tagged_tree_round_trips_and_defaults_are_literal() -> TestResult {
    let wire = json!({
        "type": "sequence", "id": "root", "children": [
            {"type": "ask", "id": "gate", "message": "Proceed?"},
            {"type": "call", "id": "read", "tool": "read_file"},
            {"type": "condition", "id": "has_result", "pointer": "/read",
             "predicate": {"op": "exists"}},
            {"type": "sample", "id": "summary", "prompt": "${this is literal}"}
        ]
    });
    let root: TreeNode = serde_json::from_value(wire)?;
    let validated = tools(root.clone())?;
    assert_eq!(validated.stats().nodes, 5);
    assert_eq!(validated.stats().depth, 2);
    assert_eq!(validated.stats().concurrent_leaves, 1);
    assert_eq!(validated.stats().leaf_attempts, 4);
    let restored: TreeNode = serde_json::from_value(serde_json::to_value(root.clone())?)?;
    assert_eq!(restored, root);
    Ok(())
}

#[test]
fn rejects_unknown_fields_variants_and_executable_conditions() {
    for wire in [
        json!({"type":"call","id":"a","tool":"read_file","idempotent":true}),
        json!({"type":"call","id":"a","tool":"read_file","_meta":{"admin":true}}),
        json!({
            "type":"condition","id":"a","pointer":"/input",
            "predicate":{"op":"eval","code":"true"}
        }),
        json!({"type":"loop","id":"a","child": {"type":"call","id":"b","tool":"x"}}),
        json!({"type":"sample","id":"a","prompt":"hello","max_tokens":-1}),
        json!({
            "type":"timeout","id":"a","timeout_ms":-1,
            "child":{"type":"call","id":"b","tool":"x"}
        }),
    ] {
        assert!(serde_json::from_value::<TreeNode>(wire).is_err());
    }
}

#[test]
fn validates_all_shape_before_any_leaf_is_run() {
    invalid(
        TreeNode::Sequence {
            id: "root".into(),
            children: vec![],
        },
        TreeErrorCode::EmptyComposite,
    );
    invalid(
        TreeNode::Sequence {
            id: "root".into(),
            children: vec![call("a"), call("a")],
        },
        TreeErrorCode::DuplicateId,
    );
    invalid(call(" "), TreeErrorCode::InvalidId);
    invalid(call("a".repeat(129)), TreeErrorCode::InvalidId);
    invalid(call("input"), TreeErrorCode::ReservedResultKey);
    let mut second = call("b");
    if let TreeNode::Call { result_key, .. } = &mut second {
        *result_key = Some("a".into());
    }
    invalid(
        TreeNode::Sequence {
            id: "root".into(),
            children: vec![call("a"), second],
        },
        TreeErrorCode::DuplicateResultKey,
    );
}

#[test]
fn checks_exact_node_and_depth_limits() -> TestResult {
    let at_nodes = TreeNode::Sequence {
        id: "root".into(),
        children: (0..127).map(|index| call(format!("leaf{index}"))).collect(),
    };
    assert_eq!(tools(at_nodes)?.stats().nodes, 128);
    invalid(
        TreeNode::Sequence {
            id: "root".into(),
            children: (0..128).map(|index| call(format!("leaf{index}"))).collect(),
        },
        TreeErrorCode::LimitExceeded,
    );
    let mut root = call("leaf");
    for depth in 1..16 {
        root = TreeNode::Timeout {
            id: format!("timeout{depth}"),
            timeout_ms: 1,
            child: Box::new(root),
        };
    }
    assert_eq!(tools(root.clone())?.stats().depth, 16);
    invalid(
        TreeNode::Timeout {
            id: "too-deep".into(),
            timeout_ms: 1,
            child: Box::new(root),
        },
        TreeErrorCode::LimitExceeded,
    );
    Ok(())
}

#[test]
fn checks_concurrency_and_retry_expanded_attempt_budget() -> TestResult {
    let parallel = |count| TreeNode::ParallelAll {
        id: "root".into(),
        children: (0..count)
            .map(|index| call(format!("leaf{index}")))
            .collect(),
    };
    assert_eq!(tools(parallel(16))?.stats().concurrent_leaves, 16);
    invalid(parallel(17), TreeErrorCode::LimitExceeded);
    let mut root = call("leaf");
    for index in 0..5 {
        root = TreeNode::Retry {
            id: format!("retry{index}"),
            attempts: 3,
            child: Box::new(root),
        };
    }
    assert_eq!(tools(root.clone())?.stats().leaf_attempts, 243);
    invalid(
        TreeNode::Retry {
            id: "too-many".into(),
            attempts: 3,
            child: Box::new(root),
        },
        TreeErrorCode::LimitExceeded,
    );
    Ok(())
}

#[test]
fn verifies_timeout_retry_and_sampling_ranges() -> TestResult {
    for timeout_ms in [0, 3_600_001] {
        invalid(
            TreeNode::Timeout {
                id: "timeout".into(),
                timeout_ms,
                child: Box::new(call("leaf")),
            },
            TreeErrorCode::InvalidTimeout,
        );
    }
    tools(TreeNode::Timeout {
        id: "timeout".into(),
        timeout_ms: 3_600_000,
        child: Box::new(call("leaf")),
    })?;
    for attempts in [0, 4] {
        invalid(
            TreeNode::Retry {
                id: "retry".into(),
                attempts,
                child: Box::new(call("leaf")),
            },
            TreeErrorCode::InvalidRetry,
        );
    }
    invalid(
        TreeNode::Sample {
            id: "sample".into(),
            prompt: "hello".into(),
            system: None,
            max_tokens: 0,
            result_key: None,
        },
        TreeErrorCode::InvalidInteraction,
    );
    Ok(())
}

#[test]
fn retries_need_trusted_idempotency_and_never_repeat_interactions() {
    let root = TreeNode::Retry {
        id: "retry".into(),
        attempts: 2,
        child: Box::new(call("leaf")),
    };
    std::assert_matches!(
        ValidatedTree::new(root, Domain::Tools { is_idempotent: &|_| false }),
        Err(error) if error.code == TreeErrorCode::UnsafeRetry
    );
    invalid(
        TreeNode::Retry {
            id: "retry".into(),
            attempts: 2,
            child: Box::new(TreeNode::Ask {
                id: "ask".into(),
                message: "Proceed?".into(),
                schema: None,
                result_key: None,
            }),
        },
        TreeErrorCode::UnsafeRetry,
    );
}

#[test]
fn every_job_must_appear_once_and_replay_is_explicit() -> TestResult {
    let job = |node: &str, job_id: &str| TreeNode::Job {
        id: node.into(),
        job_id: job_id.into(),
        result_key: None,
    };
    let declarations = BTreeMap::from([("worker".to_owned(), false)]);
    let validate = |root| {
        ValidatedTree::new(
            root,
            Domain::Agents {
                jobs: &declarations,
            },
        )
    };
    validate(job("leaf", "worker"))?;
    std::assert_matches!(
        validate(job("leaf", "missing")),
        Err(error) if error.code == TreeErrorCode::UnknownJob
    );
    std::assert_matches!(validate(TreeNode::Condition {
        id: "only-condition".into(), pointer: "/input".into(), predicate: Predicate::Exists,
    }), Err(error) if error.code == TreeErrorCode::MissingJob);
    std::assert_matches!(validate(TreeNode::Sequence {
        id: "root".into(), children: vec![job("first", "worker"), job("second", "worker")],
    }), Err(error) if error.code == TreeErrorCode::RepeatedJob);
    let retry = TreeNode::Retry {
        id: "retry".into(),
        attempts: 3,
        child: Box::new(job("leaf", "worker")),
    };
    std::assert_matches!(
        validate(retry.clone()),
        Err(error) if error.code == TreeErrorCode::UnsafeRetry
    );
    let replayable = BTreeMap::from([("worker".to_owned(), true)]);
    assert_eq!(
        ValidatedTree::new(retry, Domain::Agents { jobs: &replayable })?
            .stats()
            .leaf_attempts,
        3
    );
    std::assert_matches!(
        validate(call("not-a-job")),
        Err(error) if error.code == TreeErrorCode::WrongDomain
    );
    invalid(job("leaf", "worker"), TreeErrorCode::WrongDomain);
    Ok(())
}

#[test]
fn bindings_are_explicit_atomic_and_escape_json_pointer_keys() -> TestResult {
    let original = json!({"nested": {}, "items": [0, 1], "literal":"${input.value}"});
    let board = json!({"input":{"a/b":{"~key":null},"number":7}});
    let bindings = vec![
        Binding {
            target: "/nested/new".into(),
            source: "/input/a~1b/~0key".into(),
        },
        Binding {
            target: "/items/1".into(),
            source: "/input/number".into(),
        },
    ];
    assert_eq!(
        resolve_arguments(&original, &bindings, &board)?,
        json!({
            "nested":{"new":null}, "items":[0,7], "literal":"${input.value}"
        })
    );
    assert_eq!(original["nested"], json!({}));
    let missing = [Binding {
        target: "/nested/value".into(),
        source: "/input/missing".into(),
    }];
    std::assert_matches!(
        resolve_arguments(&original, &missing, &board),
        Err(BindingError::MissingSource { .. })
    );
    let root = [Binding {
        target: "".into(),
        source: "/input".into(),
    }];
    assert_eq!(resolve_arguments(&original, &root, &board)?, board["input"]);
    Ok(())
}

#[test]
fn malformed_overlapping_and_unwritable_bindings_fail() {
    let board = json!({"input":7});
    for target in [
        "not/pointer",
        "/a~2",
        "/items/-",
        "/items/01",
        "/items/+0",
        "/items/9",
        "/missing/child",
    ] {
        let binding = [Binding {
            target: target.into(),
            source: "/input".into(),
        }];
        assert!(
            resolve_arguments(&json!({"items":[0]}), &binding, &board).is_err(),
            "{target}"
        );
    }
    let overlap = [
        Binding {
            target: "/a".into(),
            source: "/input".into(),
        },
        Binding {
            target: "/a/b".into(),
            source: "/input".into(),
        },
    ];
    std::assert_matches!(
        resolve_arguments(&json!({"a":{}}), &overlap, &board),
        Err(BindingError::OverlappingTargets { .. })
    );
    let scalar_root = [Binding {
        target: "".into(),
        source: "/input".into(),
    }];
    std::assert_matches!(
        resolve_arguments(&json!({}), &scalar_root, &board),
        Err(BindingError::ArgumentsNotObject)
    );
}

#[test]
fn preflight_rejects_bad_binding_targets_and_condition_pointers() -> TestResult {
    let node: TreeNode = serde_json::from_value(json!({
        "type":"call", "id":"leaf", "tool":"read_file", "arguments":{"items":[]},
        "bindings":[{"target":"/items/-","source":"/input/value"}]
    }))?;
    invalid(node, TreeErrorCode::InvalidBinding);
    invalid(
        TreeNode::Condition {
            id: "check".into(),
            pointer: "/invalid~".into(),
            predicate: Predicate::Exists,
        },
        TreeErrorCode::InvalidCondition,
    );
    Ok(())
}

#[test]
fn conditions_distinguish_present_null_from_missing_and_never_interpolate() {
    let board = json!({"input":{"present":null, "text":"${literal}"}, "prior":{"ok":true}});
    assert!(Predicate::Exists.matches(&board, "/input/present"));
    assert!(!Predicate::Exists.matches(&board, "/input/missing"));
    assert!(Predicate::Equals { value: Value::Null }.matches(&board, "/input/present"));
    assert!(!Predicate::Equals { value: Value::Null }.matches(&board, "/input/missing"));
    assert!(Predicate::Equals { value: json!(true) }.matches(&board, "/prior/ok"));
    assert!(
        Predicate::Equals {
            value: json!("${literal}")
        }
        .matches(&board, "/input/text")
    );
}

#[test]
fn known_control_tools_are_not_workflow_leaves_even_when_namespaced() {
    for name in [
        "run_workflow",
        "run_synopsis",
        "spawn_agent",
        "enable_tools",
        "functions.switch_mode",
        "mcp__other__hooks_create",
    ] {
        assert!(is_control_tool(name));
        let mut root = call("leaf");
        if let TreeNode::Call { tool, .. } = &mut root {
            *tool = name.into();
        }
        invalid(root, TreeErrorCode::ForbiddenTool);
    }
    assert!(!is_control_tool("read_file"));
}
