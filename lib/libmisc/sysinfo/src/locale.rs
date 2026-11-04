//! Which locale names this host's libc accepts.
//!
//! `C.UTF-8` exists on glibc and musl but not on macOS or FreeBSD, where a
//! child given that name prints a `setlocale` warning and keeps its old locale.

use std::ffi::CString;
use std::sync::OnceLock;

const UTF8_CANDIDATES: &[&str] = &["C.UTF-8", "C.utf8", "en_US.UTF-8", "en_US.utf8", "UTF-8"];

pub const FALLBACK_LOCALE: &str = "C";

pub const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// The most preferred UTF-8 locale this host resolves, else [`FALLBACK_LOCALE`].
pub fn preferred_utf8_locale() -> &'static str {
    static LOCALE: OnceLock<&'static str> = OnceLock::new();
    LOCALE.get_or_init(|| {
        UTF8_CANDIDATES
            .iter()
            .copied()
            .find(|name| resolves(name))
            .unwrap_or(FALLBACK_LOCALE)
    })
}

pub fn resolves(name: &str) -> bool {
    let Ok(name) = CString::new(name) else {
        return false;
    };
    const MASK: libc::c_int = libc::LC_CTYPE_MASK
        | libc::LC_NUMERIC_MASK
        | libc::LC_TIME_MASK
        | libc::LC_COLLATE_MASK
        | libc::LC_MONETARY_MASK
        | libc::LC_MESSAGES_MASK;

    // SAFETY: `name` is NUL-terminated; the returned locale is freed here.
    let locale = unsafe { libc::newlocale(MASK, name.as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return false;
    }
    // SAFETY: `newlocale` returned a locale object owned by this call.
    unsafe { libc::freelocale(locale) };
    true
}

/// Replace locale variables this host rejects; resolvable choices are kept.
///
/// Covers children that inherit the environment instead of receiving an
/// explicit one. Must run during single-threaded startup.
pub fn sanitize_process_environment() {
    let fallback = preferred_utf8_locale();
    for key in LOCALE_VARS {
        let Ok(current) = std::env::var(key) else {
            continue;
        };
        if current.is_empty() || resolves(&current) {
            continue;
        }
        // SAFETY: startup fixup, before any thread reads the environment.
        unsafe { std::env::set_var(key, fallback) };
    }
}

#[cfg(test)]
mod tests;
