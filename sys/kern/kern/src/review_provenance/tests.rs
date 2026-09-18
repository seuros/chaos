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
