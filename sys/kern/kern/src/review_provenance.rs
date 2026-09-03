use chaos_ipc::openai_models::ModelFamily;
use sha2::Digest;
use sha2::Sha256;

pub(crate) const REVIEW_ACCOUNT_SUBJECT_DOMAIN: &str = "chaos/review/account/v1";
const REVIEW_MODEL_FAMILY_SUBJECT_DOMAIN: &str = "chaos/review/model-family/v1";

fn domain_separated_subject(domain: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chaos/review-subject/v1\0");
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!(
        "review-subject:v1:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

/// Unknown families have no attestable family subject. Treating every unknown
/// model as a distinct family would silently defeat diversity quorum checks.
pub(crate) fn model_family_subject(model_family: &ModelFamily) -> Option<String> {
    (!model_family.is_unknown()).then(|| {
        domain_separated_subject(REVIEW_MODEL_FAMILY_SUBJECT_DOMAIN, model_family.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_family_has_no_quorum_subject() {
        assert_eq!(model_family_subject(&ModelFamily::default()), None);
    }

    #[test]
    fn model_family_subject_is_stable_and_domain_separated() {
        let family = ModelFamily::new("anthropic");
        let subject = model_family_subject(&family).expect("known family subject");
        assert_eq!(subject, model_family_subject(&family).unwrap());
        assert_ne!(
            subject,
            domain_separated_subject(REVIEW_ACCOUNT_SUBJECT_DOMAIN, family.as_str())
        );
        assert!(!subject.contains("anthropic"));
    }
}
