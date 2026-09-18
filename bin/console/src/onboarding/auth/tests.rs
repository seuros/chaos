use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use chaos_kern::ModelProviderInfo;
use chaos_kern::WireApi;
use chaos_kern::built_in_model_providers;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use chaos_kern::auth::AuthCredentialsStoreMode;

mod key_validation;
mod moonshotai;

#[test]
fn accounts_widget_regressions() {
    auth_suite();
}

fn widget_forced_chatgpt() -> (AccountsWidget, TempDir) {
    let chaos_home = TempDir::new().unwrap();
    let chaos_home_path = chaos_home.path().to_path_buf();
    let mut model_providers = HashMap::new();
    model_providers.insert(
        "openai".to_string(),
        ModelProviderInfo::create_openai_provider(None),
    );
    let widget = AccountsWidget::new(
        FrameRequester::test_dummy(),
        chaos_home_path.clone(),
        AuthCredentialsStoreMode::File,
        Arc::new(AuthManager::new(
            chaos_home_path,
            false,
            AuthCredentialsStoreMode::File,
        )),
        &model_providers,
        None,
        Some(ForcedLoginMethod::Chatgpt),
        true,
    );
    (widget, chaos_home)
}

fn widget_with_model_providers(
    model_providers: HashMap<String, ModelProviderInfo>,
) -> (AccountsWidget, TempDir) {
    let chaos_home = TempDir::new().unwrap();
    let chaos_home_path = chaos_home.path().to_path_buf();
    let widget = AccountsWidget::new(
        FrameRequester::test_dummy(),
        chaos_home_path.clone(),
        AuthCredentialsStoreMode::File,
        Arc::new(AuthManager::new(
            chaos_home_path,
            false,
            AuthCredentialsStoreMode::File,
        )),
        &model_providers,
        None,
        None,
        true,
    );
    (widget, chaos_home)
}

pub(crate) fn auth_suite() {
    api_key_flow_disabled_when_chatgpt_forced();
    saving_api_key_is_blocked_when_chatgpt_forced();
    escape_from_provider_mode_returns_to_provider_picker();
    escape_from_single_option_provider_returns_to_provider_picker();
    provider_picker_renders_highlighted_zai_provider_when_scrolled();
    xai_provider_offers_account_connection();
    continue_in_browser_renders_osc8_hyperlink();
    mark_url_hyperlink_wraps_cyan_underlined_cells();
    mark_url_hyperlink_sanitizes_control_chars();
}

fn api_key_flow_disabled_when_chatgpt_forced() {
    let (mut widget, _tmp) = widget_forced_chatgpt();

    widget.start_api_key_entry();

    assert_eq!(
        widget.error.read().unwrap().as_deref(),
        Some(API_KEY_DISABLED_MESSAGE)
    );
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
}

fn saving_api_key_is_blocked_when_chatgpt_forced() {
    let (mut widget, _tmp) = widget_forced_chatgpt();

    widget.save_api_key("sk-test".to_string());

    assert_eq!(
        widget.error.read().unwrap().as_deref(),
        Some(API_KEY_DISABLED_MESSAGE)
    );
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
}

fn escape_from_provider_mode_returns_to_provider_picker() {
    let (mut widget, _tmp) = widget_forced_chatgpt();

    widget.open_selected_provider();
    assert!(matches!(widget.sign_in_state(), SignInState::PickMode));
    assert!(!widget.should_close_on_escape());

    widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(widget.sign_in_state(), SignInState::PickProvider));
}

fn escape_from_single_option_provider_returns_to_provider_picker() {
    let mut minimax = chaos_kern::create_oss_provider_with_base_url(
        "https://api.minimax.chat/v1",
        WireApi::ChatCompletions,
    );
    minimax.name = "MiniMax".to_string();
    minimax.env_key = Some("MINIMAX_API_KEY".to_string());

    let mut model_providers = HashMap::new();
    model_providers.insert("minimax".to_string(), minimax);

    let (mut widget, _tmp) = widget_with_model_providers(model_providers);

    widget.open_selected_provider();
    assert!(matches!(
        widget.sign_in_state(),
        SignInState::ApiKeyEntry(_)
    ));

    widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(widget.sign_in_state(), SignInState::PickProvider));
}

/// Collects all buffer cell symbols that contain the OSC 8 open sequence
/// for the given URL.  Returns the concatenated "inner" characters.
fn collect_osc8_chars(buf: &Buffer, area: Rect, url: &str) -> String {
    let open = format!("\x1B]8;;{url}\x07");
    let close = "\x1B]8;;\x07";
    let mut chars = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let sym = buf[(x, y)].symbol();
            if let Some(ch) = sym.strip_circumfix(open.as_str(), close) {
                chars.push_str(ch);
            }
        }
    }
    chars
}

fn buffer_to_text(buf: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn provider_picker_renders_highlighted_zai_provider_when_scrolled() {
    let (mut widget, _tmp) = widget_with_model_providers(built_in_model_providers());
    let zai_index = widget
        .providers
        .iter()
        .position(|provider| provider.display_name == "Z.ai")
        .expect("Z.ai provider should be connectable");
    widget.highlighted_provider = zai_index;

    let area = Rect::new(0, 0, 70, 16);
    let mut buf = Buffer::empty(area);
    widget.render_pick_provider(area, &mut buf);

    let text = buffer_to_text(&buf, area);
    assert!(
        text.contains("Z.ai"),
        "expected highlighted Z.ai provider to be visible, got: {text:?}"
    );
    assert!(
        text.contains("Showing"),
        "expected provider paging hint when list is truncated, got: {text:?}"
    );
}

fn xai_provider_offers_account_connection() {
    let (mut widget, _tmp) = widget_with_model_providers(built_in_model_providers());
    let xai_index = widget
        .providers
        .iter()
        .position(|provider| provider.id == "xai")
        .expect("xAI provider should be connectable");
    widget.highlighted_provider = xai_index;

    widget.open_selected_provider();

    assert!(matches!(widget.sign_in_state(), SignInState::PickMode));
    assert_eq!(
        widget.displayed_sign_in_options(),
        vec![SignInOption::XaiAccount, SignInOption::ApiKey]
    );
    assert_eq!(widget.highlighted_mode, SignInOption::XaiAccount);

    let area = Rect::new(0, 0, 70, 20);
    let mut buf = Buffer::empty(area);
    widget.render_pick_mode(area, &mut buf);

    let text = buffer_to_text(&buf, area);
    assert!(
        text.contains("Connect xAI account"),
        "expected xAI account option in the mode picker, got: {text:?}"
    );
}

fn continue_in_browser_renders_osc8_hyperlink() {
    let (widget, _tmp) = widget_forced_chatgpt();
    let url = "https://auth.example.com/login?state=abc123";
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
            auth_url: url.to_string(),
            cancel: None,
        });

    // Render into a narrow buffer so the URL wraps across multiple rows.
    let area = Rect::new(0, 0, 30, 20);
    let mut buf = Buffer::empty(area);
    widget.render_continue_in_browser(area, &mut buf);

    // Every character of the URL should be present as an OSC 8 cell.
    let found = collect_osc8_chars(&buf, area, url);
    assert_eq!(found, url, "OSC 8 hyperlink should cover the full URL");
}

fn mark_url_hyperlink_wraps_cyan_underlined_cells() {
    let url = "https://example.com";
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);

    // Manually write some cyan+underlined characters to simulate a rendered URL.
    for (i, ch) in "example".chars().enumerate() {
        let cell = &mut buf[(i as u16, 0)];
        cell.set_symbol(&ch.to_string());
        cell.fg = crate::theme::accent_color();
        cell.modifier = Modifier::UNDERLINED;
    }
    // Leave a plain cell that should NOT be marked.
    buf[(7, 0)].set_symbol("X");

    mark_url_hyperlink(&mut buf, area, url);

    // Each cyan+underlined cell should now carry the OSC 8 wrapper.
    let found = collect_osc8_chars(&buf, area, url);
    assert_eq!(found, "example");

    // The plain "X" cell should be untouched.
    assert_eq!(buf[(7, 0)].symbol(), "X");
}

fn mark_url_hyperlink_sanitizes_control_chars() {
    let area = Rect::new(0, 0, 10, 1);
    let mut buf = Buffer::empty(area);

    // One cyan+underlined cell to mark.
    let cell = &mut buf[(0, 0)];
    cell.set_symbol("a");
    cell.fg = crate::theme::accent_color();
    cell.modifier = Modifier::UNDERLINED;

    // URL contains ESC and BEL that could break the OSC 8 sequence.
    let malicious_url = "https://evil.com/\x1B]8;;\x07injected";
    mark_url_hyperlink(&mut buf, area, malicious_url);

    let sym = buf[(0, 0)].symbol().to_string();
    // The sanitized URL retains `]` (printable) but strips ESC and BEL.
    let sanitized = "https://evil.com/]8;;injected";
    assert!(
        sym.contains(sanitized),
        "symbol should contain sanitized URL, got: {sym:?}"
    );
    // The injected close-sequence must not survive: \x1B and \x07 are gone.
    assert!(
        !sym.contains("\x1B]8;;\x07injected"),
        "symbol must not contain raw control chars from URL"
    );
}
