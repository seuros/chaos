#![allow(clippy::unwrap_used)]

use chaos_kern::AuthManager;
use chaos_kern::ModelProviderInfo;
use chaos_kern::ProviderAuthMethod;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::CLIENT_ID;
use chaos_kern::auth::login_with_provider_api_key;
use chaos_kern::auth::read_openai_api_key_from_env;
use chaos_pam::DeviceCode;
use chaos_pam::LoginFlowCancel;
use chaos_pam::LoginFlowHandle;
use chaos_pam::LoginFlowMode;
use chaos_pam::LoginFlowUpdate;
use chaos_pam::ServerOptions;
use chaos_pam::spawn_login_flow;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use chaos_ipc::config_types::ForcedLoginMethod;
use chaos_kern::auth::AuthMode;
use std::sync::Arc;
use std::sync::RwLock;

use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::shimmer::shimmer_spans;
use crate::tui::FrameRequester;

/// Marks buffer cells that have cyan+underlined style as an OSC 8 hyperlink.
///
/// Terminal emulators recognise the OSC 8 escape sequence and treat the entire
/// marked region as a single clickable link, regardless of row wrapping.  This
/// is necessary because ratatui's cell-based rendering emits `MoveTo` at every
/// row boundary, which breaks normal terminal URL detection for long URLs that
/// wrap across multiple rows.
pub(crate) fn mark_url_hyperlink(buf: &mut Buffer, area: Rect, url: &str) {
    // Sanitize: strip any characters that could break out of the OSC 8
    // sequence (ESC or BEL) to prevent terminal escape injection from a
    // malformed or compromised upstream URL.
    let safe_url: String = url
        .chars()
        .filter(|&c| c != '\x1B' && c != '\x07')
        .collect();
    if safe_url.is_empty() {
        return;
    }

    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            // Only mark cells that carry the URL's distinctive style.
            // Use theme::cyan() so this keeps working when the palette
            // maps "cyan" to a non-standard color (e.g. LightGreen on the
            // green phosphor theme).
            if cell.fg != crate::theme::cyan() || !cell.modifier.contains(Modifier::UNDERLINED) {
                continue;
            }
            let sym = cell.symbol().to_string();
            if sym.trim().is_empty() {
                continue;
            }
            cell.set_symbol(&format!("\x1B]8;;{safe_url}\x07{sym}\x1B]8;;\x07"));
        }
    }
}
use std::path::PathBuf;

use super::onboarding_screen::StepState;

mod headless_chatgpt_login;

#[derive(Clone)]
pub(crate) enum SignInState {
    PickProvider,
    PickMode,
    ChatGptContinueInBrowser(ContinueInBrowserState),
    ChatGptDeviceCode(ContinueWithDeviceCodeState),
    ChatGptSuccessMessage,
    ChatGptSuccess,
    ApiKeyEntry(ApiKeyInputState),
    ApiKeyConfigured(AccountProvider),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AccountsCompletion {
    ConnectedProvider { provider_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOption {
    ChatGpt,
    DeviceCode,
    ApiKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountProvider {
    id: String,
    display_name: String,
    env_key: Option<String>,
    supports_chatgpt_account: bool,
    supports_api_key: bool,
}

const API_KEY_DISABLED_MESSAGE: &str = "API key connection is disabled.";

#[derive(Clone, Default)]
pub(crate) struct ApiKeyInputState {
    provider: Option<AccountProvider>,
    value: String,
    prepopulated_from_env: bool,
}

#[derive(Clone)]
/// Used to manage the lifecycle of the spawned browser sign-in flow and ensure it gets cleaned up.
pub(crate) struct ContinueInBrowserState {
    auth_url: String,
    cancel: Option<LoginFlowCancel>,
}

#[derive(Clone)]
pub(crate) struct ContinueWithDeviceCodeState {
    device_code: Option<DeviceCode>,
    cancel: Option<LoginFlowCancel>,
}

impl KeyboardHandler for AccountsWidget {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.handle_api_key_entry_key_event(&key_event) {
            return;
        }

        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let sign_in_state = self.sign_in_state();
                if matches!(sign_in_state, SignInState::PickProvider) {
                    self.move_provider_highlight(-1);
                } else {
                    self.move_highlight(/*delta*/ -1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let sign_in_state = self.sign_in_state();
                if matches!(sign_in_state, SignInState::PickProvider) {
                    self.move_provider_highlight(1);
                } else {
                    self.move_highlight(/*delta*/ 1);
                }
            }
            KeyCode::Char('1') => {
                let sign_in_state = self.sign_in_state();
                if matches!(sign_in_state, SignInState::PickProvider) {
                    self.select_provider_by_index(0);
                } else {
                    self.select_option_by_index(/*index*/ 0);
                }
            }
            KeyCode::Char('2') => {
                let sign_in_state = self.sign_in_state();
                if matches!(sign_in_state, SignInState::PickProvider) {
                    self.select_provider_by_index(1);
                } else {
                    self.select_option_by_index(/*index*/ 1);
                }
            }
            KeyCode::Char('3') => {
                let sign_in_state = self.sign_in_state();
                if matches!(sign_in_state, SignInState::PickProvider) {
                    self.select_provider_by_index(2);
                } else {
                    self.select_option_by_index(/*index*/ 2);
                }
            }
            KeyCode::Enter => {
                let sign_in_state = { (*self.sign_in_state.read().unwrap()).clone() };
                match sign_in_state {
                    SignInState::PickProvider => {
                        self.open_selected_provider();
                    }
                    SignInState::PickMode => {
                        self.handle_sign_in_option(self.highlighted_mode);
                    }
                    SignInState::ChatGptSuccessMessage => {
                        *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
                    }
                    _ => {}
                }
            }
            KeyCode::Esc => {
                tracing::info!("Esc pressed");
                let mut sign_in_state = self.sign_in_state.write().unwrap();
                match &*sign_in_state {
                    SignInState::ChatGptContinueInBrowser(state) => {
                        if let Some(cancel) = &state.cancel {
                            cancel.cancel();
                        }
                        *sign_in_state = self.back_destination_for_selected_provider();
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::ChatGptDeviceCode(state) => {
                        if let Some(cancel) = &state.cancel {
                            cancel.cancel();
                        }
                        *sign_in_state = self.back_destination_for_selected_provider();
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::PickMode => {
                        *sign_in_state = SignInState::PickProvider;
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::ApiKeyEntry(_) => {
                        *sign_in_state = self.back_destination_for_selected_provider();
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        let _ = self.handle_api_key_entry_paste(pasted);
    }
}

#[derive(Clone)]
pub(crate) struct AccountsWidget {
    pub request_frame: FrameRequester,
    pub highlighted_provider: usize,
    pub highlighted_mode: SignInOption,
    pub error: Arc<RwLock<Option<String>>>,
    pub sign_in_state: Arc<RwLock<SignInState>>,
    pub chaos_home: PathBuf,
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub auth_manager: Arc<AuthManager>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub animations_enabled: bool,
    pub providers: Vec<AccountProvider>,
    pub selected_provider_id: Arc<RwLock<Option<String>>>,
}

impl AccountsWidget {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        request_frame: FrameRequester,
        chaos_home: PathBuf,
        cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
        auth_manager: Arc<AuthManager>,
        model_providers: &std::collections::HashMap<String, ModelProviderInfo>,
        forced_chatgpt_workspace_id: Option<String>,
        forced_login_method: Option<ForcedLoginMethod>,
        animations_enabled: bool,
    ) -> Self {
        let providers = Self::build_connectable_providers(model_providers, forced_login_method);
        Self {
            request_frame,
            highlighted_provider: 0,
            highlighted_mode: SignInOption::ChatGpt,
            error: Arc::new(RwLock::new(None)),
            sign_in_state: Arc::new(RwLock::new(SignInState::PickProvider)),
            chaos_home,
            cli_auth_credentials_store_mode,
            auth_manager,
            forced_chatgpt_workspace_id,
            animations_enabled,
            providers,
            selected_provider_id: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn sign_in_state(&self) -> SignInState {
        self.sign_in_state.read().unwrap().clone()
    }

    pub(crate) fn completion(&self) -> Option<AccountsCompletion> {
        match self.sign_in_state() {
            SignInState::ChatGptSuccess => {
                self.selected_provider()
                    .map(|provider| AccountsCompletion::ConnectedProvider {
                        provider_id: provider.id,
                    })
            }
            SignInState::ApiKeyConfigured(provider) => {
                Some(AccountsCompletion::ConnectedProvider {
                    provider_id: provider.id,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn should_close_on_escape(&self) -> bool {
        matches!(
            self.sign_in_state(),
            SignInState::PickProvider
                | SignInState::ChatGptSuccess
                | SignInState::ApiKeyConfigured(_)
        )
    }

    fn back_destination_for_selected_provider(&self) -> SignInState {
        let option_count = self
            .selected_provider()
            .map(|provider| self.provider_sign_in_options(&provider).len())
            .unwrap_or_default();
        if option_count <= 1 {
            SignInState::PickProvider
        } else {
            SignInState::PickMode
        }
    }

    fn set_error(&self, error: Option<String>) {
        *self.error.write().unwrap() = error;
    }

    fn error_message(&self) -> Option<String> {
        self.error.read().unwrap().clone()
    }

    fn build_connectable_providers(
        model_providers: &std::collections::HashMap<String, ModelProviderInfo>,
        forced_login_method: Option<ForcedLoginMethod>,
    ) -> Vec<AccountProvider> {
        let mut providers = model_providers
            .iter()
            .filter_map(|(id, provider)| {
                let supports_chatgpt_account = provider
                    .supports_auth_method(ProviderAuthMethod::ChatgptAccount)
                    && !matches!(forced_login_method, Some(ForcedLoginMethod::Api));
                let supports_api_key = provider.supports_auth_method(ProviderAuthMethod::ApiKey)
                    && !matches!(forced_login_method, Some(ForcedLoginMethod::Chatgpt));
                if !supports_chatgpt_account && !supports_api_key {
                    return None;
                }
                Some(AccountProvider {
                    id: id.clone(),
                    display_name: provider.name.clone(),
                    env_key: provider.env_key.clone(),
                    supports_chatgpt_account,
                    supports_api_key,
                })
            })
            .collect::<Vec<_>>();
        providers.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        providers
    }

    fn selected_provider(&self) -> Option<AccountProvider> {
        let selected = self.selected_provider_id.read().unwrap().clone();
        if let Some(selected) = selected {
            return self.providers.iter().find(|p| p.id == selected).cloned();
        }
        self.providers.get(self.highlighted_provider).cloned()
    }

    fn set_selected_provider(&self, provider: &AccountProvider) {
        *self.selected_provider_id.write().unwrap() = Some(provider.id.clone());
    }

    fn is_api_login_allowed(&self) -> bool {
        self.selected_provider()
            .map(|provider| provider.supports_api_key)
            .unwrap_or(false)
    }

    fn is_chatgpt_login_allowed(&self) -> bool {
        self.selected_provider()
            .map(|provider| provider.supports_chatgpt_account)
            .unwrap_or(false)
    }

    fn provider_sign_in_options(&self, provider: &AccountProvider) -> Vec<SignInOption> {
        let mut options = Vec::new();
        if provider.supports_chatgpt_account {
            options.push(SignInOption::ChatGpt);
            options.push(SignInOption::DeviceCode);
        }
        if provider.supports_api_key {
            options.push(SignInOption::ApiKey);
        }
        options
    }

    fn displayed_sign_in_options(&self) -> Vec<SignInOption> {
        self.selected_provider()
            .map(|provider| self.provider_sign_in_options(&provider))
            .unwrap_or_default()
    }

    fn selectable_sign_in_options(&self) -> Vec<SignInOption> {
        self.displayed_sign_in_options()
    }

    fn move_highlight(&mut self, delta: isize) {
        let options = self.selectable_sign_in_options();
        if options.is_empty() {
            return;
        }

        let current_index = options
            .iter()
            .position(|option| *option == self.highlighted_mode)
            .unwrap_or(0);
        let next_index =
            (current_index as isize + delta).rem_euclid(options.len() as isize) as usize;
        let next_option = options[next_index];
        if self.highlighted_mode != next_option {
            self.highlighted_mode = next_option;
            self.request_frame.schedule_frame();
        }
    }

    fn move_provider_highlight(&mut self, delta: isize) {
        if self.providers.is_empty() {
            return;
        }
        let len = self.providers.len() as isize;
        self.highlighted_provider =
            (self.highlighted_provider as isize + delta).rem_euclid(len) as usize;
        self.request_frame.schedule_frame();
    }

    fn select_provider_by_index(&mut self, index: usize) {
        if let Some(provider) = self.providers.get(index).cloned() {
            self.highlighted_provider = index;
            self.set_selected_provider(&provider);
            self.open_provider(provider);
        }
    }

    fn open_selected_provider(&mut self) {
        if let Some(provider) = self
            .selected_provider()
            .or_else(|| self.providers.get(self.highlighted_provider).cloned())
        {
            self.open_provider(provider);
        }
    }

    fn open_provider(&mut self, provider: AccountProvider) {
        self.set_selected_provider(&provider);
        let options = self.provider_sign_in_options(&provider);
        if options.len() == 1 {
            self.highlighted_mode = options[0];
            self.handle_sign_in_option(options[0]);
            return;
        }
        self.highlighted_mode = options.first().copied().unwrap_or(SignInOption::ApiKey);
        *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        self.set_error(None);
        self.request_frame.schedule_frame();
    }

    fn select_option_by_index(&mut self, index: usize) {
        let options = self.displayed_sign_in_options();
        if let Some(option) = options.get(index).copied() {
            self.highlighted_mode = option;
            self.handle_sign_in_option(option);
        }
    }

    fn handle_sign_in_option(&mut self, option: SignInOption) {
        match option {
            SignInOption::ChatGpt => {
                if self.is_chatgpt_login_allowed() {
                    self.start_chatgpt_account_connection();
                }
            }
            SignInOption::DeviceCode => {
                if self.is_chatgpt_login_allowed() {
                    self.start_device_code_connection();
                }
            }
            SignInOption::ApiKey => {
                if self.is_api_login_allowed() {
                    self.start_api_key_entry();
                } else {
                    self.disallow_api_login();
                }
            }
        }
    }

    fn disallow_api_login(&mut self) {
        self.highlighted_mode = SignInOption::ChatGpt;
        self.set_error(Some(API_KEY_DISABLED_MESSAGE.to_string()));
        *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        self.request_frame.schedule_frame();
    }

    fn render_pick_provider(&self, area: Rect, buf: &mut Buffer) {
        let error = self.error_message();
        let error_lines = usize::from(error.is_some()) * 2;
        let base_reserved_lines = 4 + error_lines;
        let mut max_visible_providers =
            (area.height.saturating_sub(base_reserved_lines as u16) as usize / 3).max(1);
        let show_window_hint = self.providers.len() > max_visible_providers;
        if show_window_hint {
            max_visible_providers =
                (area.height.saturating_sub((base_reserved_lines + 1) as u16) as usize / 3).max(1);
        }

        let max_start = self.providers.len().saturating_sub(max_visible_providers);
        let mut start = self
            .highlighted_provider
            .saturating_sub(max_visible_providers.saturating_sub(1) / 2);
        start = start.min(max_start);
        let end = (start + max_visible_providers).min(self.providers.len());

        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                "  ".into(),
                "Choose a provider account to connect".into(),
            ]),
            "".into(),
        ];
        for (idx, provider) in self
            .providers
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let selected = self.highlighted_provider == idx;
            let caret = if selected { ">" } else { " " };
            let title = format!(
                "{} {}",
                provider.display_name,
                if provider.id == "openai" {
                    "(ChatGPT or API key)"
                } else {
                    ""
                }
            )
            .trim()
            .to_string();
            let description = if provider.supports_chatgpt_account && provider.supports_api_key {
                "ChatGPT account or API key"
            } else if provider.supports_chatgpt_account {
                "ChatGPT account"
            } else {
                "API key"
            };
            let line1 = if selected {
                Line::from(vec![
                    format!("{caret} {}. ", idx + 1).cyan().dim(),
                    title.clone().cyan(),
                ])
            } else {
                format!("  {}. {title}", idx + 1).into()
            };
            let line2 = if selected {
                Line::from(format!("     {description}"))
                    .fg(crate::theme::cyan())
                    .add_modifier(Modifier::DIM)
            } else {
                Line::from(format!("     {description}"))
                    .style(Style::default().add_modifier(Modifier::DIM))
            };
            lines.push(line1);
            lines.push(line2);
            lines.push("".into());
        }
        if show_window_hint {
            lines.push(
                format!(
                    "  Showing {}-{} of {} providers",
                    start + 1,
                    end,
                    self.providers.len()
                )
                .dim()
                .into(),
            );
        }
        lines.push("  Use ↑/↓ (or j/k) to choose".dim().into());
        lines.push("  Press Enter to continue".dim().into());
        if let Some(err) = error {
            lines.push("".into());
            lines.push(err.red().into());
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_pick_mode(&self, area: Rect, buf: &mut Buffer) {
        let provider = self.selected_provider();
        let provider_label = provider
            .as_ref()
            .map(|provider| provider.display_name.as_str())
            .unwrap_or("provider");
        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                "  ".into(),
                format!("Choose how to connect {provider_label}").into(),
            ]),
            Line::from(vec![
                "  ".into(),
                "Account connections are provider-specific.".into(),
            ]),
            "".into(),
        ];

        let create_mode_item = |idx: usize,
                                selected_mode: SignInOption,
                                text: &str,
                                description: &str|
         -> Vec<Line<'static>> {
            let is_selected = self.highlighted_mode == selected_mode;
            let caret = if is_selected { ">" } else { " " };

            let line1 = if is_selected {
                Line::from(vec![
                    format!("{caret} {index}. ", index = idx + 1).cyan().dim(),
                    text.to_string().cyan(),
                ])
            } else {
                format!("  {index}. {text}", index = idx + 1).into()
            };

            let line2 = if is_selected {
                Line::from(format!("     {description}"))
                    .fg(crate::theme::cyan())
                    .add_modifier(Modifier::DIM)
            } else {
                Line::from(format!("     {description}"))
                    .style(Style::default().add_modifier(Modifier::DIM))
            };

            vec![line1, line2]
        };

        let chatgpt_description = if !self.is_chatgpt_login_allowed() {
            "ChatGPT account connection is unavailable"
        } else {
            "Usage included with Plus, Pro, Business, and Enterprise plans"
        };
        let device_code_description = "Sign in from another device with a one-time code";

        for (idx, option) in self.displayed_sign_in_options().into_iter().enumerate() {
            match option {
                SignInOption::ChatGpt => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Connect ChatGPT account",
                        chatgpt_description,
                    ));
                }
                SignInOption::DeviceCode => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Connect with Device Code",
                        device_code_description,
                    ));
                }
                SignInOption::ApiKey => {
                    let provider_api_key_label = provider
                        .as_ref()
                        .map(|provider| format!("Provide {} API key", provider.display_name))
                        .unwrap_or_else(|| "Provide API key".to_string());
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        &provider_api_key_label,
                        "Pay for what you use",
                    ));
                }
            }
            lines.push("".into());
        }

        lines.push("  Use ↑/↓ (or j/k) to choose".dim().into());
        lines.push("  Press Enter to continue".dim().into());
        lines.push("  Press Esc to go back".dim().into());
        if let Some(err) = self.error_message() {
            lines.push("".into());
            lines.push(err.red().into());
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_continue_in_browser(&self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec!["  ".into()];
        if self.animations_enabled {
            // Schedule a follow-up frame to keep the shimmer animation going.
            self.request_frame
                .schedule_frame_in(std::time::Duration::from_millis(100));
            spans.extend(shimmer_spans("Finish signing in via your browser"));
        } else {
            spans.push("Finish signing in via your browser".into());
        }
        let mut lines = vec![spans.into(), "".into()];

        let sign_in_state = self.sign_in_state.read().unwrap();
        let auth_url = if let SignInState::ChatGptContinueInBrowser(state) = &*sign_in_state
            && !state.auth_url.is_empty()
        {
            lines.push("  If the link doesn't open automatically, open the following link to authenticate:".into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                state.auth_url.as_str().cyan().underlined(),
            ]));
            lines.push("".into());
            lines.push(Line::from(vec![
                "  On a remote or headless machine? Press Esc and choose ".into(),
                "Sign in with Device Code".cyan(),
                ".".into(),
            ]));
            lines.push("".into());
            Some(state.auth_url.clone())
        } else {
            None
        };

        lines.push("  Press Esc to cancel".dim().into());
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);

        // Wrap cyan+underlined URL cells with OSC 8 so the terminal treats
        // the entire region as a single clickable hyperlink.
        if let Some(url) = &auth_url {
            mark_url_hyperlink(buf, area, url);
        }
    }

    fn render_chatgpt_success_message(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Authenticated".fg(crate::theme::green()).into(),
            "".into(),
            "  Before you proceed:".into(),
            "".into(),
            "  Tools:        none destructive by default.".into(),
            "  Permissions:  none granted by default.".into(),
            "  Mistakes:     yours, not the model's.".into(),
            "".into(),
            "  Every tool the model sees, you mounted.".dim().into(),
            "  Every action it takes, you permitted.".dim().into(),
            "  The harness warns. It does not intervene.".dim().into(),
            "".into(),
            "  Press Enter to continue".fg(crate::theme::cyan()).into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_chatgpt_success(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with your ChatGPT account"
                .fg(crate::theme::green())
                .into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_configured(&self, area: Rect, buf: &mut Buffer, provider: &AccountProvider) {
        let lines = vec![
            format!("✓ {} API key configured", provider.display_name)
                .fg(crate::theme::green())
                .into(),
            "".into(),
            format!(
                "  Chaos will use your stored {} credentials for this provider.",
                provider.display_name
            )
            .into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_entry(&self, area: Rect, buf: &mut Buffer, state: &ApiKeyInputState) {
        let provider_name = state
            .provider
            .as_ref()
            .map(|provider| provider.display_name.as_str())
            .unwrap_or("provider");
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let mut intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                format!("Use your own {provider_name} API key").bold(),
            ]),
            "".into(),
            "  Paste or type your API key below. It will be stored locally in auth.json.".into(),
            "".into(),
        ];
        if state.prepopulated_from_env {
            if let Some(provider) = state.provider.as_ref()
                && let Some(env_key) = provider.env_key.as_deref()
            {
                intro_lines.push(format!("  Detected {env_key} environment variable.").into());
            }
            intro_lines.push(
                "  Paste a different key if you prefer to use another account."
                    .dim()
                    .into(),
            );
            intro_lines.push("".into());
        }
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if state.value.is_empty() {
            vec!["Paste or type your API key".dim()].into()
        } else {
            Line::from(state.value.clone())
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(format!("{provider_name} API key"))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(crate::theme::cyan())),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            "  Press Enter to save".dim().into(),
            "  Press Esc to go back".dim().into(),
        ];
        if let Some(error) = self.error_message() {
            footer_lines.push("".into());
            footer_lines.push(error.red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn handle_api_key_entry_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let mut should_save: Option<String> = None;
        let mut should_request_frame = false;
        let api_key_escape_destination = self.back_destination_for_selected_provider();

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::ApiKeyEntry(state) = &mut *guard {
                match key_event.code {
                    KeyCode::Esc => {
                        *guard = api_key_escape_destination;
                        self.set_error(None);
                        should_request_frame = true;
                    }
                    KeyCode::Enter => {
                        let trimmed = state.value.trim().to_string();
                        if trimmed.is_empty() {
                            self.set_error(Some("API key cannot be empty".to_string()));
                            should_request_frame = true;
                        } else {
                            should_save = Some(trimmed);
                        }
                    }
                    KeyCode::Backspace => {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        } else {
                            state.value.pop();
                        }
                        self.set_error(None);
                        should_request_frame = true;
                    }
                    KeyCode::Char(c)
                        if key_event.kind == KeyEventKind::Press
                            && !key_event.modifiers.contains(KeyModifiers::SUPER)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        }
                        state.value.push(c);
                        self.set_error(None);
                        should_request_frame = true;
                    }
                    _ => {}
                }
                // handled; let guard drop before potential save
            } else {
                return false;
            }
        }

        if let Some(api_key) = should_save {
            self.save_api_key(api_key);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn handle_api_key_entry_paste(&mut self, pasted: String) -> bool {
        let trimmed = pasted.trim();
        if trimmed.is_empty() {
            return false;
        }

        let mut guard = self.sign_in_state.write().unwrap();
        if let SignInState::ApiKeyEntry(state) = &mut *guard {
            if state.prepopulated_from_env {
                state.value = trimmed.to_string();
                state.prepopulated_from_env = false;
            } else {
                state.value.push_str(trimmed);
            }
            self.set_error(None);
        } else {
            return false;
        }

        drop(guard);
        self.request_frame.schedule_frame();
        true
    }

    fn start_api_key_entry(&mut self) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        let Some(provider) = self.selected_provider() else {
            self.set_error(Some("Choose a provider first.".to_string()));
            *self.sign_in_state.write().unwrap() = SignInState::PickProvider;
            self.request_frame.schedule_frame();
            return;
        };
        self.set_error(None);
        let prefill_from_env = provider
            .env_key
            .as_deref()
            .and_then(|env_key| std::env::var(env_key).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                (provider.id == "openai")
                    .then(read_openai_api_key_from_env)
                    .flatten()
            });
        let mut guard = self.sign_in_state.write().unwrap();
        match &mut *guard {
            SignInState::ApiKeyEntry(state) => {
                state.provider = Some(provider);
                if state.value.is_empty() {
                    if let Some(prefill) = prefill_from_env {
                        state.value = prefill;
                        state.prepopulated_from_env = true;
                    } else {
                        state.prepopulated_from_env = false;
                    }
                }
            }
            _ => {
                *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                    provider: Some(provider),
                    value: prefill_from_env.clone().unwrap_or_default(),
                    prepopulated_from_env: prefill_from_env.is_some(),
                });
            }
        }
        drop(guard);
        self.request_frame.schedule_frame();
    }

    fn save_api_key(&mut self, api_key: String) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        let provider = match self.selected_provider() {
            Some(provider) => provider,
            None => {
                self.set_error(Some("Choose a provider first.".to_string()));
                *self.sign_in_state.write().unwrap() = SignInState::PickProvider;
                self.request_frame.schedule_frame();
                return;
            }
        };
        match login_with_provider_api_key(
            &self.chaos_home,
            &provider.id,
            &api_key,
            self.cli_auth_credentials_store_mode,
        ) {
            Ok(()) => {
                self.set_error(None);
                self.auth_manager.reload();
                *self.sign_in_state.write().unwrap() = SignInState::ApiKeyConfigured(provider);
            }
            Err(err) => {
                self.set_error(Some(format!("Failed to save API key: {err}")));
                let mut guard = self.sign_in_state.write().unwrap();
                if let SignInState::ApiKeyEntry(existing) = &mut *guard {
                    existing.provider = Some(provider.clone());
                    if existing.value.is_empty() {
                        existing.value.push_str(&api_key);
                    }
                    existing.prepopulated_from_env = false;
                } else {
                    *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                        provider: Some(provider),
                        value: api_key,
                        prepopulated_from_env: false,
                    });
                }
            }
        }

        self.request_frame.schedule_frame();
    }

    fn handle_existing_chatgpt_connection(&mut self) -> bool {
        let Some(selected_provider_id) = self.selected_provider().map(|provider| provider.id)
        else {
            return false;
        };
        if self
            .auth_manager
            .auth_for_provider(&selected_provider_id)
            .as_ref()
            .is_some_and(|auth| auth.auth_mode() == AuthMode::Chatgpt)
        {
            *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
            self.request_frame.schedule_frame();
            true
        } else {
            false
        }
    }

    /// Kicks off the ChatGPT account flow and keeps the UI state consistent with the attempt.
    fn start_chatgpt_account_connection(&mut self) {
        // If we're already connected with ChatGPT, don't start a new flow –
        // just proceed to the success message flow.
        if self.handle_existing_chatgpt_connection() {
            return;
        }

        self.set_error(None);
        let opts = ServerOptions::new(
            self.chaos_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );
        let handle = spawn_login_flow(opts, LoginFlowMode::Browser);
        let cancel = handle.cancel_handle();
        *self.sign_in_state.write().unwrap() =
            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                auth_url: String::new(),
                cancel: Some(cancel),
            });
        self.request_frame.schedule_frame();
        self.consume_chatgpt_account_flow(handle);
    }

    fn start_device_code_connection(&mut self) {
        if self.handle_existing_chatgpt_connection() {
            return;
        }

        self.set_error(None);
        let mut opts = ServerOptions::new(
            self.chaos_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );
        opts.open_browser = false;
        let handle = spawn_login_flow(
            opts,
            LoginFlowMode::DeviceCode {
                allow_browser_fallback: true,
            },
        );
        let cancel = handle.cancel_handle();
        *self.sign_in_state.write().unwrap() =
            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState {
                device_code: None,
                cancel: Some(cancel),
            });
        self.request_frame.schedule_frame();
        self.consume_chatgpt_account_flow(handle);
    }

    fn consume_chatgpt_account_flow(&mut self, mut handle: LoginFlowHandle) {
        let sign_in_state = self.sign_in_state.clone();
        let error = self.error.clone();
        let request_frame = self.request_frame.clone();
        let auth_manager = self.auth_manager.clone();
        let fallback_state = self.back_destination_for_selected_provider();

        tokio::spawn(async move {
            let cancel = handle.cancel_handle();
            while let Some(update) = handle.recv().await {
                match update {
                    LoginFlowUpdate::DeviceCodePending => {
                        *error.write().unwrap() = None;
                        *sign_in_state.write().unwrap() =
                            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState {
                                device_code: None,
                                cancel: Some(cancel.clone()),
                            });
                    }
                    LoginFlowUpdate::DeviceCodeUnsupported => {}
                    LoginFlowUpdate::BrowserOpened { auth_url, .. } => {
                        *error.write().unwrap() = None;
                        *sign_in_state.write().unwrap() =
                            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                                auth_url,
                                cancel: Some(cancel.clone()),
                            });
                    }
                    LoginFlowUpdate::DeviceCodeReady { device_code } => {
                        *error.write().unwrap() = None;
                        *sign_in_state.write().unwrap() =
                            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState {
                                device_code: Some(device_code),
                                cancel: Some(cancel.clone()),
                            });
                    }
                    LoginFlowUpdate::Succeeded { .. } => {
                        *error.write().unwrap() = None;
                        auth_manager.reload();
                        *sign_in_state.write().unwrap() = SignInState::ChatGptSuccessMessage;
                    }
                    LoginFlowUpdate::Failed { message } => {
                        *error.write().unwrap() = Some(message);
                        *sign_in_state.write().unwrap() = fallback_state.clone();
                    }
                    LoginFlowUpdate::Cancelled => {
                        *error.write().unwrap() = None;
                        *sign_in_state.write().unwrap() = fallback_state.clone();
                    }
                }
                request_frame.schedule_frame();
            }
        });
    }
}

impl StepStateProvider for AccountsWidget {
    fn get_step_state(&self) -> StepState {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickProvider
            | SignInState::PickMode
            | SignInState::ApiKeyEntry(_)
            | SignInState::ChatGptContinueInBrowser(_)
            | SignInState::ChatGptDeviceCode(_)
            | SignInState::ChatGptSuccessMessage => StepState::InProgress,
            SignInState::ChatGptSuccess | SignInState::ApiKeyConfigured(_) => StepState::Complete,
        }
    }
}

impl WidgetRef for AccountsWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickProvider => {
                self.render_pick_provider(area, buf);
            }
            SignInState::PickMode => {
                self.render_pick_mode(area, buf);
            }
            SignInState::ChatGptContinueInBrowser(_) => {
                self.render_continue_in_browser(area, buf);
            }
            SignInState::ChatGptDeviceCode(state) => {
                headless_chatgpt_login::render_device_code_login(self, area, buf, state);
            }
            SignInState::ChatGptSuccessMessage => {
                self.render_chatgpt_success_message(area, buf);
            }
            SignInState::ChatGptSuccess => {
                self.render_chatgpt_success(area, buf);
            }
            SignInState::ApiKeyEntry(state) => {
                self.render_api_key_entry(area, buf, state);
            }
            SignInState::ApiKeyConfigured(provider) => {
                self.render_api_key_configured(area, buf, provider);
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
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

    #[test]
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

    #[test]
    fn escape_from_provider_mode_returns_to_provider_picker() {
        let (mut widget, _tmp) = widget_forced_chatgpt();

        widget.open_selected_provider();
        assert!(matches!(widget.sign_in_state(), SignInState::PickMode));
        assert!(!widget.should_close_on_escape());

        widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(widget.sign_in_state(), SignInState::PickProvider));
    }

    #[test]
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
                if let Some(rest) = sym.strip_prefix(open.as_str())
                    && let Some(ch) = rest.strip_suffix(close)
                {
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

    #[test]
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

    #[test]
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

    #[test]
    fn mark_url_hyperlink_wraps_cyan_underlined_cells() {
        let url = "https://example.com";
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);

        // Manually write some cyan+underlined characters to simulate a rendered URL.
        for (i, ch) in "example".chars().enumerate() {
            let cell = &mut buf[(i as u16, 0)];
            cell.set_symbol(&ch.to_string());
            cell.fg = crate::theme::cyan();
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

    #[test]
    fn mark_url_hyperlink_sanitizes_control_chars() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        // One cyan+underlined cell to mark.
        let cell = &mut buf[(0, 0)];
        cell.set_symbol("a");
        cell.fg = crate::theme::cyan();
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
}
