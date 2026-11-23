use std::collections::BTreeSet;

use chaos_mcp_runtime::GuestError;
use chaos_mcp_runtime::ReadResourceRequestParams;
use chaos_mcp_runtime::ReadResourceResult;
use rama::futures::future::join_all;

use crate::chaos::Session;
use crate::function_tool::FunctionCallError;

pub(super) async fn read_external_resource(
    session: &Session,
    server: &str,
    uri: &str,
) -> anyhow::Result<ReadResourceResult> {
    session
        .read_resource(
            server,
            ReadResourceRequestParams {
                uri: uri.to_string(),
                meta: None,
            },
        )
        .await
}

pub(super) async fn read_advertised_resource(
    session: &Session,
    uri: &str,
) -> Result<(String, ReadResourceResult), FunctionCallError> {
    let registry = &session.services.mcp_registry;
    let (resources, templates) = tokio::join!(
        registry.list_all_resources(),
        registry.list_all_resource_templates()
    );
    let candidates: BTreeSet<_> = resources
        .into_iter()
        .filter(|(_, resources)| resources.iter().any(|resource| resource.uri == uri))
        .map(|(server, _)| server)
        .chain(
            templates
                .into_iter()
                .filter(|(_, templates)| {
                    templates
                        .iter()
                        .any(|template| template_may_match(&template.uri_template, uri))
                })
                .map(|(server, _)| server),
        )
        .collect();

    // An advertised template does not promise every expansion exists.
    let reads = join_all(candidates.into_iter().map(|server| async move {
        let result = read_external_resource(session, &server, uri).await;
        (server, result)
    }))
    .await;
    select_resource(uri, reads)
}

fn select_resource(
    uri: &str,
    reads: Vec<(String, anyhow::Result<ReadResourceResult>)>,
) -> Result<(String, ReadResourceResult), FunctionCallError> {
    // MCP resources/read uses this JSON-RPC code for a missing resource.
    const RESOURCE_NOT_FOUND: i32 = -32002;
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for (server, result) in reads {
        match result {
            Ok(result) => found.push((server, result)),
            Err(err)
                if matches!(
                    err.downcast_ref::<GuestError>(),
                    Some(GuestError::Server {
                        code: RESOURCE_NOT_FOUND,
                        ..
                    })
                ) => {}
            Err(err) => failures.push(format!("{server}: {err:#}")),
        }
    }
    if found.len() > 1 {
        let servers = found
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        return Err(FunctionCallError::RespondToModel(format!(
            "resource URI {uri:?} is ambiguous; served by {}. Specify `server`.",
            servers.join(", ")
        )));
    }
    if !failures.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "could not resolve resource URI {uri:?}: {}. Specify `server` to read directly.",
            failures.join("; ")
        )));
    }
    found.pop().ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "no MCP server was found serving resource URI {uri:?}; specify `server` for unlisted resources"
        ))
    })
}

// Use RFC 6570's mandatory literals to narrow discovery; the server is the
// authority on the expression syntax, variables, and which expansions exist.
fn template_may_match(template: &str, uri: &str) -> bool {
    let Some((prefix, expressions)) = template.split_once('{') else {
        return template == uri;
    };
    // Don't send a URI to a server that hasn't advertised at least its scheme.
    if !prefix.contains(':') {
        return false;
    }
    let Some(uri) = uri.strip_prefix(prefix) else {
        return false;
    };
    let Some((expressions, suffix)) = expressions.rsplit_once('}') else {
        return false;
    };
    let Some(mut rest) = uri.strip_suffix(suffix) else {
        return false;
    };
    for (_, literal) in expressions
        .split('{')
        .map_while(|part| part.split_once('}'))
    {
        let Some((_, tail)) = rest.split_once(literal) else {
            return false;
        };
        rest = tail;
    }
    true
}

#[cfg(test)]
mod tests;
