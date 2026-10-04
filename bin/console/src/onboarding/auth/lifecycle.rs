use super::{
    AccountProvider, ApiKeyInputState, ContinueInBrowserState, ContinueWithDeviceCodeState,
    SignInState, XaiDeviceCodeLoginState,
};
use state_machines::state_machine;

impl std::fmt::Debug for ApiKeyInputState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKeyInputState(..)")
    }
}

impl std::fmt::Debug for ContinueInBrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContinueInBrowserState(..)")
    }
}

impl std::fmt::Debug for ContinueWithDeviceCodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContinueWithDeviceCodeState(..)")
    }
}

impl std::fmt::Debug for XaiDeviceCodeLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XaiDeviceCodeLoginState(..)")
    }
}

state_machine! {
    name: AccountSignIn,
    dynamic: true,
    initial: PickProvider,
    states: [
        superstate View {
            state PickProvider, state PickMode, state BrowserWaiting(ContinueInBrowserState),
            state DeviceWaiting(ContinueWithDeviceCodeState), state XaiWaiting(XaiDeviceCodeLoginState),
            state SuccessMessage, state Succeeded, state KeyEditing(ApiKeyInputState),
            state KeyConfigured(AccountProvider),
        }
    ],
    events {
        providers { transition: { from: View, to: PickProvider } }
        modes { transition: { from: View, to: PickMode } }
        browser {
            payload: Option<ContinueInBrowserState>,
            transition: { from: View, to: BrowserWaiting, data: own_browser }
        }
        device {
            payload: Option<ContinueWithDeviceCodeState>,
            transition: { from: View, to: DeviceWaiting, data: own_device }
        }
        xai {
            payload: Option<XaiDeviceCodeLoginState>,
            transition: { from: View, to: XaiWaiting, data: own_xai }
        }
        edit_key {
            payload: Option<ApiKeyInputState>,
            transition: { from: View, to: KeyEditing, data: own_key }
        }
        configure_key {
            payload: Option<AccountProvider>,
            transition: { from: View, to: KeyConfigured, data: own_provider }
        }
        already_connected { transition: { from: View, to: Succeeded } }
        succeed {
            transition: { from: BrowserWaiting, to: SuccessMessage }
            transition: { from: DeviceWaiting, to: SuccessMessage }
            transition: { from: XaiWaiting, to: SuccessMessage }
        }
        acknowledge { transition: { from: SuccessMessage, to: Succeeded } }
    }
}

fn own<T>(data: &mut Option<T>) -> T {
    data.take()
        .unwrap_or_else(|| unreachable!("transition owns its data"))
}

impl<C, S> AccountSignIn<C, S> {
    fn own_browser(&self, data: &mut Option<ContinueInBrowserState>) -> ContinueInBrowserState {
        own(data)
    }
    fn own_device(
        &self,
        data: &mut Option<ContinueWithDeviceCodeState>,
    ) -> ContinueWithDeviceCodeState {
        own(data)
    }
    fn own_xai(&self, data: &mut Option<XaiDeviceCodeLoginState>) -> XaiDeviceCodeLoginState {
        own(data)
    }
    fn own_key(&self, data: &mut Option<ApiKeyInputState>) -> ApiKeyInputState {
        own(data)
    }
    fn own_provider(&self, data: &mut Option<AccountProvider>) -> AccountProvider {
        own(data)
    }
}

pub(crate) struct SignIn {
    machine: DynamicAccountSignIn<()>,
}

impl Default for SignIn {
    fn default() -> Self {
        Self {
            machine: DynamicAccountSignIn::new(()),
        }
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        self.cancel_pending_login();
    }
}

impl SignIn {
    pub(super) fn snapshot(&self) -> SignInState {
        match self.machine.current_state() {
            AccountSignInState::PickProvider => SignInState::PickProvider,
            AccountSignInState::PickMode => SignInState::PickMode,
            AccountSignInState::BrowserWaiting => SignInState::ChatGptContinueInBrowser(
                self.machine
                    .browser_waiting_data()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("browser owns data")),
            ),
            AccountSignInState::DeviceWaiting => SignInState::ChatGptDeviceCode(
                self.machine
                    .device_waiting_data()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("device owns data")),
            ),
            AccountSignInState::XaiWaiting => SignInState::XaiDeviceCode(
                self.machine
                    .xai_waiting_data()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("xai owns data")),
            ),
            AccountSignInState::SuccessMessage => SignInState::ChatGptSuccessMessage,
            AccountSignInState::Succeeded => SignInState::ChatGptSuccess,
            AccountSignInState::KeyEditing => SignInState::ApiKeyEntry(
                self.machine
                    .key_editing_data()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("key owns data")),
            ),
            AccountSignInState::KeyConfigured => SignInState::ApiKeyConfigured(
                self.machine
                    .key_configured_data()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("provider owns data")),
            ),
        }
    }

    pub(super) fn key_mut(&mut self) -> Option<&mut ApiKeyInputState> {
        self.machine.key_editing_data_mut()
    }

    pub(super) fn xai_mut(&mut self) -> Option<&mut XaiDeviceCodeLoginState> {
        self.machine.xai_waiting_data_mut()
    }

    pub(super) fn cancel_pending_login(&self) {
        if let Some(cancel) = self
            .machine
            .browser_waiting_data()
            .and_then(|data| data.cancel.as_ref())
        {
            cancel.cancel();
        }
        if let Some(cancel) = self
            .machine
            .device_waiting_data()
            .and_then(|data| data.cancel.as_ref())
        {
            cancel.cancel();
        }
        if let Some(cancel) = self
            .machine
            .xai_waiting_data()
            .and_then(|data| data.cancel.as_ref())
        {
            cancel.cancel();
        }
    }

    pub(super) fn transition(&mut self, next: SignInState) -> bool {
        if !matches!(
            &next,
            SignInState::ChatGptContinueInBrowser(_)
                | SignInState::ChatGptDeviceCode(_)
                | SignInState::XaiDeviceCode(_)
        ) {
            self.cancel_pending_login();
        }
        let event = match next {
            SignInState::PickProvider => AccountSignInEvent::Providers,
            SignInState::PickMode => AccountSignInEvent::Modes,
            SignInState::ChatGptContinueInBrowser(data) => AccountSignInEvent::Browser(Some(data)),
            SignInState::ChatGptDeviceCode(data) => AccountSignInEvent::Device(Some(data)),
            SignInState::XaiDeviceCode(data) => AccountSignInEvent::Xai(Some(data)),
            SignInState::ChatGptSuccessMessage => AccountSignInEvent::Succeed,
            SignInState::ChatGptSuccess
                if self.machine.current_state() == AccountSignInState::SuccessMessage =>
            {
                AccountSignInEvent::Acknowledge
            }
            SignInState::ChatGptSuccess => AccountSignInEvent::AlreadyConnected,
            SignInState::ApiKeyEntry(data) => AccountSignInEvent::EditKey(Some(data)),
            SignInState::ApiKeyConfigured(data) => AccountSignInEvent::ConfigureKey(Some(data)),
        };
        self.machine.handle(event).is_ok()
    }
}

#[cfg(test)]
mod tests;
