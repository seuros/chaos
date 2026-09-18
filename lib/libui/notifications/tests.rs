use super::detect_backend;
use chaos_kern::config::types::NotificationMethod;
use std::ffi::OsString;

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

pub(crate) fn notifications_suite() {
    selects_osc9_method();
    selects_bel_method();
    auto_prefers_bel_without_hints();
    auto_uses_osc9_for_iterm();
}
#[cfg(test)]
fn selects_osc9_method() {
    assert!(matches!(
        detect_backend(NotificationMethod::Osc9),
        super::DesktopNotificationBackend::Osc9(_)
    ));
}

#[cfg(test)]
fn selects_bel_method() {
    assert!(matches!(
        detect_backend(NotificationMethod::Bel),
        super::DesktopNotificationBackend::Bel(_)
    ));
}

fn auto_prefers_bel_without_hints() {
    let _term = EnvVarGuard::remove("TERM");
    let _term_program = EnvVarGuard::remove("TERM_PROGRAM");
    let _iterm = EnvVarGuard::remove("ITERM_SESSION_ID");
    let _wt = EnvVarGuard::remove("WT_SESSION");
    assert!(matches!(
        detect_backend(NotificationMethod::Auto),
        super::DesktopNotificationBackend::Bel(_)
    ));
}

fn auto_uses_osc9_for_iterm() {
    let _term = EnvVarGuard::remove("TERM");
    let _term_program = EnvVarGuard::remove("TERM_PROGRAM");
    let _iterm = EnvVarGuard::set("ITERM_SESSION_ID", "abc");
    let _wt = EnvVarGuard::remove("WT_SESSION");
    assert!(matches!(
        detect_backend(NotificationMethod::Auto),
        super::DesktopNotificationBackend::Osc9(_)
    ));
}
