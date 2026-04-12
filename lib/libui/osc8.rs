//! OSC 8 hyperlink registry.
//!
//! URLs are stored in a process-wide table; `register` hands out a
//! `Color::Rgb` sentinel that callers stamp into `Style::underline_color` on
//! link spans, and `insert_history::write_spans` resolves the sentinel back
//! to the URL at emit time. Id 0 is reserved. The id space is 24-bit and
//! saturates on overflow.

#![allow(clippy::disallowed_methods)]

use std::env;
use std::sync::Mutex;
use std::sync::OnceLock;

use ratatui::style::Color;
use supports_hyperlinks::Stream;

static REGISTRY: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

fn registry() -> &'static Mutex<Vec<String>> {
    REGISTRY.get_or_init(|| Mutex::new(vec![String::new()]))
}

/// Register `url` and return the sentinel to stamp into `underline_color`.
/// Duplicate URLs reuse the same slot.
pub fn register(url: &str) -> Color {
    let mut table = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = match table.iter().position(|existing| existing == url) {
        Some(idx) => idx,
        None => {
            table.push(url.to_string());
            table.len() - 1
        }
    };
    let id = id.min(0x00FF_FFFF) as u32;
    let r = ((id >> 16) & 0xFF) as u8;
    let g = ((id >> 8) & 0xFF) as u8;
    let b = (id & 0xFF) as u8;
    Color::Rgb(r, g, b)
}

/// Resolve a sentinel back to its URL, or `None` if it isn't one.
pub fn lookup(color: Color) -> Option<String> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    let id = ((r as usize) << 16) | ((g as usize) << 8) | (b as usize);
    if id == 0 {
        return None;
    }
    let table = registry().lock().ok()?;
    table.get(id).cloned()
}

/// Whether OSC 8 should be emitted. Honors `CHAOS_OSC8`, then
/// `supports-hyperlinks`, then opts in under tmux.
pub fn enabled() -> bool {
    #[cfg(test)]
    {
        if let Some(v) = TEST_OVERRIDE.with(std::cell::Cell::get) {
            return v;
        }
    }
    *ENABLED.get_or_init(|| {
        if let Ok(val) = env::var("CHAOS_OSC8") {
            return matches!(
                val.as_str(),
                "1" | "true" | "yes" | "on" | "TRUE" | "YES" | "ON"
            );
        }
        if supports_hyperlinks::on(Stream::Stdout) {
            return true;
        }
        env::var("TERM_PROGRAM").as_deref() == Ok("tmux") || env::var("TMUX").is_ok()
    })
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Scoped test override for [`enabled`]. Restores the previous value on drop.
#[cfg(test)]
pub fn with_enabled<R>(val: bool, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<bool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            let prev = self.0;
            TEST_OVERRIDE.with(|cell| cell.set(prev));
        }
    }
    let prev = TEST_OVERRIDE.with(|cell| cell.replace(Some(val)));
    let _guard = Guard(prev);
    f()
}

/// OSC 8 opener: `ESC ] 8 ; ; URL ST`.
pub fn open(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\")
}

/// OSC 8 closer: `ESC ] 8 ; ; ST`.
pub fn close() -> &'static str {
    "\x1b]8;;\x1b\\"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_roundtrips_url_via_lookup() {
        let color = register("https://example.com/a");
        assert_eq!(lookup(color).as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn register_deduplicates_identical_urls() {
        let a = register("https://example.com/dedup");
        let b = register("https://example.com/dedup");
        assert_eq!(a, b);
    }

    #[test]
    fn lookup_ignores_non_rgb_and_zero_sentinel() {
        assert!(lookup(Color::Reset).is_none());
        assert!(lookup(Color::Rgb(0, 0, 0)).is_none());
    }

    #[test]
    fn open_and_close_use_st_terminator() {
        assert_eq!(
            open("https://example.com"),
            "\x1b]8;;https://example.com\x1b\\"
        );
        assert_eq!(close(), "\x1b]8;;\x1b\\");
    }
}
