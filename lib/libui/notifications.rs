mod bel;
mod osc9;

use std::env;
use std::io;

use bel::BelBackend;
use chaos_kern::config::types::NotificationMethod;
use osc9::Osc9Backend;

#[derive(Debug)]
pub enum DesktopNotificationBackend {
    Osc9(Osc9Backend),
    Bel(BelBackend),
}

impl DesktopNotificationBackend {
    pub fn for_method(method: NotificationMethod) -> Self {
        match method {
            NotificationMethod::Auto => {
                if supports_osc9() {
                    Self::Osc9(Osc9Backend)
                } else {
                    Self::Bel(BelBackend)
                }
            }
            NotificationMethod::Osc9 => Self::Osc9(Osc9Backend),
            NotificationMethod::Bel => Self::Bel(BelBackend),
        }
    }

    pub fn method(&self) -> NotificationMethod {
        match self {
            DesktopNotificationBackend::Osc9(_) => NotificationMethod::Osc9,
            DesktopNotificationBackend::Bel(_) => NotificationMethod::Bel,
        }
    }

    pub fn notify(&mut self, message: &str) -> io::Result<()> {
        match self {
            DesktopNotificationBackend::Osc9(backend) => backend.notify(message),
            DesktopNotificationBackend::Bel(backend) => backend.notify(message),
        }
    }
}

pub fn detect_backend(method: NotificationMethod) -> DesktopNotificationBackend {
    DesktopNotificationBackend::for_method(method)
}

fn supports_osc9() -> bool {
    if env::var_os("WT_SESSION").is_some() {
        return false;
    }
    // Prefer TERM_PROGRAM when present, but keep fallbacks for shells/launchers
    // that don't set it (e.g., tmux/ssh) to avoid regressing OSC 9 support.
    if matches!(
        env::var("TERM_PROGRAM").ok().as_deref(),
        Some("WezTerm" | "ghostty")
    ) {
        return true;
    }
    // iTerm still provides a strong session signal even when TERM_PROGRAM is missing.
    if env::var_os("ITERM_SESSION_ID").is_some() {
        return true;
    }
    // TERM-based hints cover kitty/wezterm setups without TERM_PROGRAM.
    matches!(
        env::var("TERM").ok().as_deref(),
        Some("xterm-kitty" | "wezterm" | "wezterm-mux")
    )
}

#[cfg(test)]
pub(crate) mod tests;
