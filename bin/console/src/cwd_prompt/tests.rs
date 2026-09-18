use super::*;
#[cfg(feature = "vt100-tests")]
use crate::test_backend::VT100Backend;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
#[cfg(feature = "vt100-tests")]
use ratatui::Terminal;

fn new_prompt() -> CwdPromptScreen {
    CwdPromptScreen::new(
        FrameRequester::test_dummy(),
        CwdPromptAction::Resume,
        "/Users/example/current".to_string(),
        "/Users/example/session".to_string(),
    )
}

pub(crate) fn cwd_prompt_suite() {
    #[cfg(feature = "vt100-tests")]
    cwd_prompt_snapshot();
    #[cfg(feature = "vt100-tests")]
    cwd_prompt_fork_snapshot();
    cwd_prompt_key_handling_selects_default_current_and_exit();
}

#[cfg(feature = "vt100-tests")]
fn cwd_prompt_snapshot() {
    let screen = new_prompt();
    let mut terminal = Terminal::new(VT100Backend::new(80, 14)).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&screen, frame.area()))
        .expect("render cwd prompt");
    insta::assert_snapshot!("cwd_prompt_modal", terminal.backend());
}

#[cfg(feature = "vt100-tests")]
fn cwd_prompt_fork_snapshot() {
    let screen = CwdPromptScreen::new(
        FrameRequester::test_dummy(),
        CwdPromptAction::Fork,
        "/Users/example/current".to_string(),
        "/Users/example/session".to_string(),
    );
    let mut terminal = Terminal::new(VT100Backend::new(80, 14)).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&screen, frame.area()))
        .expect("render cwd prompt");
    insta::assert_snapshot!("cwd_prompt_fork_modal", terminal.backend());
}

fn cwd_prompt_key_handling_selects_default_current_and_exit() {
    let mut screen = new_prompt();
    screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(screen.selection(), Some(CwdSelection::Session));

    let mut screen = new_prompt();
    screen.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(screen.selection(), Some(CwdSelection::Current));

    let mut screen = new_prompt();
    screen.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(screen.selection(), None);
    assert!(screen.is_done());
}
