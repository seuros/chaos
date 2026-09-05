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
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_branch_resource_defaults_and_percent_decoding() {
        let defaults = parse_branch_resource_uri("git://branches").expect("default URI");
        assert!(matches!(defaults.scope, BranchScope::All));
        assert!(defaults.contains.is_none());

        let filtered = parse_branch_resource_uri("git://branches?scope=local&contains=feature%2F")
            .expect("filtered URI");
        assert!(matches!(filtered.scope, BranchScope::Local));
        assert_eq!(filtered.contains.as_deref(), Some("feature/"));
    }

    #[test]
    fn rejects_unknown_duplicate_and_invalid_query_parameters() {
        assert!(parse_branch_resource_uri("git://branches?scope=other").is_err());
        assert!(parse_branch_resource_uri("git://branches?scope=all&scope=local").is_err());
        assert!(parse_branch_resource_uri("git://branches?unexpected=true").is_err());
    }

    #[test]
    fn branch_resource_applies_scope_and_contains_filters() {
        let temp = tempdir().expect("tempdir");
        git(temp.path(), &["init", "-b", "main"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        fs::write(temp.path().join("file.txt"), "initial\n").expect("write file");
        git(temp.path(), &["add", "file.txt"]);
        git(temp.path(), &["commit", "-m", "initial"]);
        git(temp.path(), &["branch", "feature/local"]);
        git(temp.path(), &["branch", "bugfix"]);
        git(
            temp.path(),
            &["update-ref", "refs/remotes/origin/feature/remote", "HEAD"],
        );

        let result = read_branches(
            temp.path(),
            BranchResourceParams {
                scope: BranchScope::Local,
                contains: Some("feature/".to_string()),
            },
        )
        .expect("read branches");
        assert!(
            !result.text.contains('\n'),
            "model-facing JSON must be compact"
        );
        let value: serde_json::Value =
            serde_json::from_str(&result.text).expect("parse resource JSON");
        assert_eq!(value["local"], serde_json::json!(["feature/local"]));
        assert_eq!(value["remote"], serde_json::json!([]));
        assert_eq!(value["current"], "main");
    }
}
