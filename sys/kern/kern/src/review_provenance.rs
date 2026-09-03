use chaos_ipc::openai_models::ModelFamily;
use sha2::Digest;
use sha2::Sha256;

pub(crate) const REVIEW_ACCOUNT_SUBJECT_DOMAIN: &str = "chaos/review/account/v1";
const REVIEW_MODEL_FAMILY_SUBJECT_DOMAIN: &str = "chaos/review/model-family/v1";
const REVIEW_RUN_SUBJECT_DOMAIN: &str = "chaos/review/run/v1";
const REVIEWER_ATTEMPT_SUBJECT_DOMAIN: &str = "chaos/review/attempt/v1";

fn domain_separated_subject(domain: &str, prefix: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chaos/review-subject/v1\0");
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{prefix}{}",
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
        domain_separated_subject(
            REVIEW_MODEL_FAMILY_SUBJECT_DOMAIN,
            "review-subject:v1:",
            model_family.as_str(),
        )
    })
}

pub(crate) fn review_run_subject(run_id: &str) -> String {
    domain_separated_subject(REVIEW_RUN_SUBJECT_DOMAIN, "review-run:v1:", run_id)
}

pub(crate) fn reviewer_attempt_subject(attempt_id: &str) -> String {
    domain_separated_subject(
        REVIEWER_ATTEMPT_SUBJECT_DOMAIN,
        "reviewer-attempt:v1:",
        attempt_id,
    )
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
            domain_separated_subject(
                REVIEW_ACCOUNT_SUBJECT_DOMAIN,
                "review-subject:v1:",
                family.as_str()
            )
        );
        assert!(!subject.contains("anthropic"));
    }

    #[test]
    fn run_and_attempt_subjects_are_opaque_and_domain_separated() {
        let id = "01991234-1234-7123-8123-123456789abc";
        let run = review_run_subject(id);
        let attempt = reviewer_attempt_subject(id);
        assert!(run.starts_with("review-run:v1:"));
        assert!(attempt.starts_with("reviewer-attempt:v1:"));
        assert_ne!(run, attempt);
        assert!(!run.contains(id));
        assert!(!attempt.contains(id));
    }
}
