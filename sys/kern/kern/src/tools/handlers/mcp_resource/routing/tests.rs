use super::*;

const URI: &str = "memo://notes/welcome";

fn content() -> ReadResourceResult {
    super::super::inline_text_resource_result(URI, "Welcome".into(), "text/plain")
}

fn missing() -> anyhow::Error {
    anyhow::Error::new(GuestError::Server {
        code: -32002,
        message: "Memo not found".into(),
        data: None,
    })
    .context("resources/read failed")
}

#[test]
fn only_successful_reads_count_as_serving_a_uri() {
    let (server, _) = select_resource(
        URI,
        vec![
            ("alpha".into(), Ok(content())),
            ("beta".into(), Err(missing())),
        ],
    )
    .expect("one server serves the memo");
    assert_eq!(server, "alpha");

    for (peer_result, expected) in [
        (Ok(content()), "ambiguous; served by alpha, beta"),
        (Err(anyhow::anyhow!("timed out")), "beta: timed out"),
    ] {
        let err = select_resource(
            URI,
            vec![
                ("alpha".into(), Ok(content())),
                ("beta".into(), peer_result),
            ],
        )
        .expect_err("a unique provider cannot be established");
        assert!(err.to_string().contains(expected), "{err}");
        assert!(err.to_string().contains("`server`"), "{err}");
    }

    let err = select_resource(URI, vec![("beta".into(), Err(missing()))])
        .expect_err("no server serves the memo");
    assert!(err.to_string().contains("no MCP server was found"), "{err}");
}

#[test]
fn discovery_uses_template_literals_to_narrow_candidates() {
    for (template, uri, expected) in [
        ("memo://notes/{+name}", URI, true),
        ("memo://notes/{name}", URI, true),
        ("crate://{name}", URI, false),
        (
            "skipper://pr/{number}/checks{?repo}",
            "skipper://pr/12/checks",
            true,
        ),
        (
            "skipper://pr/{number}/checks{?repo}",
            "skipper://pr/12/files",
            false,
        ),
        (
            "skipper://pr/{number}/checks",
            "skipper://pr/12/checks/extra",
            false,
        ),
        ("{+uri}", URI, false),
    ] {
        assert_eq!(
            template_may_match(template, uri),
            expected,
            "{template} {uri}"
        );
    }
}
