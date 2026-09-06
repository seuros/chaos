//! Exact model identities, revision 1.

use chaos_ipc::openai_models::ModelFamily;

/// Pin endpoints to prevent identity reuse by custom gateways.
pub(crate) fn family(
    provider_id: &str,
    base_url: Option<&str>,
    model_id: &str,
) -> Option<ModelFamily> {
    match (provider_id, base_url, model_id) {
        ("charm", Some("https://hyper.charm.land/v1"), "deepseek-v4-pro") => {
            Some(ModelFamily::new("deepseek"))
        }
        _ => None,
    }
}
