use std::path::Path;

use chaos_traits::catalog::CatalogResourceDriver;
use chaos_traits::catalog::CatalogResourceDriverFuture;
use chaos_traits::catalog::CatalogResourceRequest;
use chaos_traits::catalog::CatalogResourceResult;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum BranchScope {
    #[default]
    All,
    Local,
    Remote,
}

#[derive(Debug, Clone)]
struct BranchResourceParams {
    scope: BranchScope,
    contains: Option<String>,
}

pub(crate) struct GitResourceDriver;

fn parse_branch_resource_uri(uri: &str) -> Result<BranchResourceParams, String> {
    let url = url::Url::parse(uri).map_err(|error| format!("invalid Git resource URI: {error}"))?;
    if url.scheme() != "git"
        || url.host_str() != Some("branches")
        || !matches!(url.path(), "" | "/")
        || url.fragment().is_some()
    {
        return Err(format!("unsupported Git resource URI: {uri}"));
    }

    let mut scope = None;
    let mut contains = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "scope" if scope.is_none() => {
                scope = Some(match value.as_ref() {
                    "all" => BranchScope::All,
                    "local" => BranchScope::Local,
                    "remote" => BranchScope::Remote,
                    other => {
                        return Err(format!(
                            "invalid branch scope {other:?}; expected all, local, or remote"
                        ));
                    }
                });
            }
            "contains" if contains.is_none() => {
                contains = (!value.is_empty()).then(|| value.into_owned());
            }
            "scope" | "contains" => {
                return Err(format!("duplicate Git resource query parameter {key:?}"));
            }
            other => {
                return Err(format!("unknown Git resource query parameter {other:?}"));
            }
        }
    }

    Ok(BranchResourceParams {
        scope: scope.unwrap_or_default(),
        contains,
    })
}

fn read_branches(
    cwd: &Path,
    params: BranchResourceParams,
) -> Result<CatalogResourceResult, String> {
    let mut info = crate::branches(cwd).map_err(|error| error.to_string())?;
    match params.scope {
        BranchScope::All => {}
        BranchScope::Local => info.remote.clear(),
        BranchScope::Remote => info.local.clear(),
    }
    if let Some(contains) = params.contains {
        info.local.retain(|branch| branch.contains(&contains));
        info.remote.retain(|branch| branch.contains(&contains));
    }

    Ok(CatalogResourceResult {
        text: serde_json::to_string(&info).map_err(|error| error.to_string())?,
        mime_type: "application/json".to_string(),
    })
}

impl CatalogResourceDriver for GitResourceDriver {
    fn matches(&self, uri: &str) -> bool {
        url::Url::parse(uri).is_ok_and(|url| {
            url.scheme() == "git"
                && url.host_str() == Some("branches")
                && matches!(url.path(), "" | "/")
        })
    }

    fn read_resource(&self, request: CatalogResourceRequest) -> CatalogResourceDriverFuture<'_> {
        Box::pin(async move {
            let params = parse_branch_resource_uri(&request.uri)?;
            crate::tools::execute_blocking(request.cwd, params, read_branches).await
        })
    }
}

#[cfg(test)]
mod tests;
