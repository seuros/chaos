//! Live context pressure: per-window bookkeeping of how loaded the model's
//! context is since the last distillation. A window spans the history between
//! two distillations; the baseline records the input-token prefill observed at
//! the start of the window so allotment scopes can measure growth rather than
//! total size.

use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deferral {
    pub model: String,
    pub effective_context_window: i64,
    pub ceiling: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactRequest {
    pub model: String,
    pub effective_context_window: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Control {
    #[default]
    Normal,
    Deferred(Deferral),
    CompactRequested(CompactRequest),
}

/// Input-token baseline for the current window. A server-observed value comes
/// from real usage reported by the provider and always wins over an estimate;
/// once observed it is never overwritten within the same window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Baseline {
    ServerObserved(i64),
    Estimated(i64),
}

impl Baseline {
    pub fn tokens(self) -> i64 {
        match self {
            Baseline::ServerObserved(tokens) | Baseline::Estimated(tokens) => tokens,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Window {
    window_number: u64,
    first_window_id: Uuid,
    previous_window_id: Option<Uuid>,
    window_id: Uuid,
    baseline: Option<Baseline>,
    reminder_claimed: bool,
    deferral_used: bool,
    control: Control,
}

impl Window {
    pub fn new() -> Self {
        let window_id = Uuid::now_v7();
        Self {
            window_number: 0,
            first_window_id: window_id,
            previous_window_id: None,
            window_id,
            baseline: None,
            reminder_claimed: false,
            deferral_used: false,
            control: Control::Normal,
        }
    }

    /// Reconstruct a pressure window after resume. UUID identity remains
    /// process-local; the compaction count is the durable window identity.
    pub fn from_number(window_number: u64) -> Self {
        let mut window = Self::new();
        window.window_number = window_number;
        window
    }

    /// Rotates to a fresh window after a distillation installs replacement
    /// history: the baseline and reminder claim reset, the window ids chain.
    pub fn advance(&mut self) {
        self.previous_window_id = Some(self.window_id);
        self.window_id = Uuid::now_v7();
        self.window_number += 1;
        self.baseline = None;
        self.reminder_claimed = false;
        self.deferral_used = false;
        self.control = Control::Normal;
    }

    /// Records the server-reported input-token prefill for this window. Only
    /// the first observation per window is kept.
    pub fn observe_server_baseline(&mut self, input_tokens: i64) {
        if !matches!(self.baseline, Some(Baseline::ServerObserved(_))) {
            self.baseline = Some(Baseline::ServerObserved(input_tokens));
        }
    }

    /// Records an estimated baseline; ignored once any baseline exists.
    pub fn set_estimated_baseline(&mut self, tokens: i64) {
        if self.baseline.is_none() {
            self.baseline = Some(Baseline::Estimated(tokens));
        }
    }

    pub fn baseline(&self) -> Option<Baseline> {
        self.baseline
    }

    /// Claims the once-per-window reminder; returns true only for the first
    /// claim after each `advance`.
    pub fn claim_reminder(&mut self) -> bool {
        !std::mem::replace(&mut self.reminder_claimed, true)
    }

    pub fn reminder_claimed(&self) -> bool {
        self.reminder_claimed
    }

    pub fn control(&self) -> &Control {
        &self.control
    }

    pub fn defer(&mut self, deferral: Deferral) {
        self.deferral_used = true;
        self.control = Control::Deferred(deferral);
    }

    pub fn deferral_used(&self) -> bool {
        self.deferral_used
    }

    pub fn request_compaction(&mut self, request: CompactRequest) {
        self.control = Control::CompactRequested(request);
    }

    pub fn clear_compaction_request(&mut self) {
        if matches!(self.control, Control::CompactRequested(_)) {
            self.control = Control::Normal;
        }
    }

    pub fn restore_control(&mut self, control: Control) {
        if matches!(control, Control::Deferred(_)) {
            self.deferral_used = true;
        }
        self.control = control;
    }

    pub fn mark_deferral_used(&mut self) {
        self.deferral_used = true;
    }

    pub fn reset_control(&mut self) {
        self.deferral_used = false;
        self.control = Control::Normal;
    }

    pub fn window_number(&self) -> u64 {
        self.window_number
    }

    pub fn window_id(&self) -> Uuid {
        self.window_id
    }

    pub fn first_window_id(&self) -> Uuid {
        self.first_window_id
    }

    pub fn previous_window_id(&self) -> Option<Uuid> {
        self.previous_window_id
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_lifecycle_chains_ids_and_resets_state() {
        let mut window = Window::new();
        assert_eq!(window.window_number(), 0);
        assert_eq!(window.first_window_id(), window.window_id());
        assert_eq!(window.previous_window_id(), None);

        // Estimated baseline yields to the first server observation; further
        // observations and estimates within the window are ignored.
        window.set_estimated_baseline(100);
        assert_eq!(window.baseline(), Some(Baseline::Estimated(100)));
        window.observe_server_baseline(250);
        assert_eq!(window.baseline(), Some(Baseline::ServerObserved(250)));
        window.observe_server_baseline(999);
        window.set_estimated_baseline(1);
        assert_eq!(window.baseline(), Some(Baseline::ServerObserved(250)));

        assert!(window.claim_reminder());
        assert!(!window.claim_reminder());

        let first_id = window.window_id();
        window.advance();
        assert_eq!(window.window_number(), 1);
        assert_eq!(window.previous_window_id(), Some(first_id));
        assert_eq!(window.first_window_id(), first_id);
        assert_ne!(window.window_id(), first_id);
        assert_eq!(window.baseline(), None);
        assert_eq!(window.control(), &Control::Normal);
        assert!(!window.deferral_used());
        assert!(window.claim_reminder());
    }

    #[test]
    fn reconstructed_window_keeps_number_but_uses_fresh_uuid() {
        let window = Window::from_number(7);
        assert_eq!(window.window_number(), 7);
        assert_eq!(window.first_window_id(), window.window_id());
        assert_eq!(window.previous_window_id(), None);
    }

    #[test]
    fn advance_resets_compaction_control() {
        let mut window = Window::new();
        window.defer(Deferral {
            model: "model".to_string(),
            effective_context_window: 100,
            ceiling: 80,
        });
        assert!(window.deferral_used());
        window.advance();
        assert_eq!(window.control(), &Control::Normal);
        assert!(!window.deferral_used());
    }
}
