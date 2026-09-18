#[cfg(feature = "vt100-tests")]
use crate::test_backend::VT100Backend;

use super::*;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
#[cfg(feature = "vt100-tests")]
use ratatui::Terminal;
use std::path::PathBuf;
use tempfile::TempDir;

pub(crate) fn trust_directory_suite() {
    release_event_does_not_change_selection();
    #[cfg(feature = "vt100-tests")]
    renders_snapshot_for_git_repo();
}

fn release_event_does_not_change_selection() {
    let chaos_home = TempDir::new().expect("temp home");
    let mut widget = TrustDirectoryWidget {
        chaos_home: chaos_home.path().to_path_buf(),
        cwd: PathBuf::from("."),
        show_windows_create_sandbox_hint: false,
        should_quit: false,
        selection: None,
        highlighted: TrustDirectorySelection::Quit,
        error: None,
    };

    let release = KeyEvent {
        kind: KeyEventKind::Release,
        ..KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    };
    widget.handle_key_event(release);
    assert_eq!(widget.selection, None);

    let press = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    widget.handle_key_event(press);
    assert!(widget.should_quit);
}

#[cfg(feature = "vt100-tests")]
fn renders_snapshot_for_git_repo() {
    let chaos_home = TempDir::new().expect("temp home");
    let widget = TrustDirectoryWidget {
        chaos_home: chaos_home.path().to_path_buf(),
        cwd: PathBuf::from("/workspace/project"),
        show_windows_create_sandbox_hint: false,
        should_quit: false,
        selection: None,
        highlighted: TrustDirectorySelection::Trust,
        error: None,
    };

    let mut terminal = Terminal::new(VT100Backend::new(70, 14)).expect("terminal");
    terminal
        .draw(|f| (&widget).render(f.area(), f.buffer_mut()))
        .expect("draw");

    insta::assert_snapshot!(terminal.backend());
}
