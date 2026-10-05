mod bel;
mod osc9;

use std::io;

use bel::BelBackend;
use chaos_kern::config::types::NotificationMethod;
use chaos_sysinfo::terminal::{TerminalInfo, TerminalName, terminal_info};
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
                if supports_osc9(&terminal_info()) {
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

fn supports_osc9(terminal: &TerminalInfo) -> bool {
    !terminal.in_tmux()
        && matches!(
            terminal.name,
            TerminalName::WezTerm
                | TerminalName::Ghostty
                | TerminalName::Iterm2
                | TerminalName::Kitty
        )
}

#[cfg(test)]
pub(crate) mod tests;
