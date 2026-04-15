//! Cross-platform helper for preventing idle sleep while a turn is running.
//!
//! Platform-specific behavior:
//! - macOS: Uses native IOKit power assertions instead of spawning `caffeinate`.
//! - Linux: Spawns `systemd-inhibit` or `gnome-session-inhibit` while active.
//! - Other platforms: No-op backend.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod dummy;
#[cfg(target_os = "linux")]
mod linux_inhibitor;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use dummy as imp;
#[cfg(target_os = "linux")]
use linux_inhibitor as imp;
#[cfg(target_os = "macos")]
use macos as imp;

/// Keeps the machine awake while a turn is in progress when enabled.
#[derive(Debug)]
pub struct SleepInhibitor {
    enabled: bool,
    turn_running: bool,
    platform: imp::SleepInhibitor,
}

impl SleepInhibitor {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            turn_running: false,
            platform: imp::SleepInhibitor::new(),
        }
    }

    /// Update the active turn state; turns sleep prevention on/off as needed.
    pub fn set_turn_running(&mut self, turn_running: bool) {
        self.turn_running = turn_running;
        if !self.enabled {
            self.release();
            return;
        }

        if turn_running {
            self.acquire();
        } else {
            self.release();
        }
    }

    fn acquire(&mut self) {
        self.platform.acquire();
    }

    fn release(&mut self) {
        self.platform.release();
    }

    /// Return the latest turn-running state requested by the caller.
    pub fn is_turn_running(&self) -> bool {
        self.turn_running
    }
}
