use super::*;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn appending_prefix_rules_creates_directories_and_preserves_one_line_separator() {
    let rule = "prefix_rule {pattern={\"echo\", \"Hello, world!\"}, decision=\"allow\"}\n";
    let seed = "prefix_rule {pattern={\"ls\"}, decision=\"allow\"}";
    for existing in [None, Some(seed.to_string()), Some(format!("{seed}\n"))] {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.decrees");
        if let Some(contents) = &existing {
            std::fs::create_dir_all(policy_path.parent().unwrap()).expect("create policy dir");
            std::fs::write(&policy_path, contents).expect("write seed rule");
        }
        blocking_append_allow_prefix_rule(
            &policy_path,
            &[String::from("echo"), String::from("Hello, world!")],
        )
        .expect("append rule");
        let expected = if existing.is_some() {
            format!("{seed}\n{rule}")
        } else {
            rule.to_string()
        };
        assert_eq!(std::fs::read_to_string(&policy_path).unwrap(), expected);
    }
}

#[test]
fn appending_network_rules_normalizes_hosts_and_preserves_existing_prefix_rules() {
    for has_prefix in [false, true] {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.decrees");
        if has_prefix {
            blocking_append_allow_prefix_rule(&policy_path, &[String::from("curl")])
                .expect("append prefix rule");
        }
        blocking_append_network_rule(
            &policy_path,
            "Api.GitHub.com",
            NetworkRuleProtocol::Https,
            Decision::Allow,
            Some("Allow https_connect access to api.github.com"),
        )
        .expect("append network rule");
        let prefix = if has_prefix {
            "prefix_rule {pattern={\"curl\"}, decision=\"allow\"}\n"
        } else {
            ""
        };
        assert_eq!(
            std::fs::read_to_string(&policy_path).unwrap(),
            format!(
                "{prefix}network_rule {{host=\"api.github.com\", protocol=\"https\", decision=\"allow\", justification=\"Allow https_connect access to api.github.com\"}}\n"
            )
        );
    }
}

#[test]
fn rejects_wildcard_network_rule_host() {
    let tmp = tempdir().expect("create temp dir");
    let policy_path = tmp.path().join("rules").join("default.decrees");
    let err = blocking_append_network_rule(
        &policy_path,
        "*.example.com",
        NetworkRuleProtocol::Https,
        Decision::Allow,
        None,
    )
    .expect_err("wildcards should be rejected");
    assert_eq!(
        err.to_string(),
        "invalid network rule: invalid rule: network_rule host must be a specific host; wildcards are not allowed"
    );
}
