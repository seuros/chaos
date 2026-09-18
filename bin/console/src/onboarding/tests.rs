pub(crate) fn onboarding_suite() {
    super::auth::tests::auth_suite();
    super::trust_directory::tests::trust_directory_suite();
    super::welcome::tests::welcome_renders_text_at_top();
}
