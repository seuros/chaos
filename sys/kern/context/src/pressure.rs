//! Live context pressure: per-window bookkeeping of how loaded the model's
//! context is since the last distillation. A window spans the history between
//! two distillations; the baseline records the input-token prefill observed at
//! the start of the window so allotment scopes can measure growth rather than
//! total size.

use uuid::Uuid;

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
        }
    }

    /// Rotates to a fresh window after a distillation installs replacement
    /// history: the baseline and reminder claim reset, the window ids chain.
    pub fn advance(&mut self) {
        self.previous_window_id = Some(self.window_id);
        self.window_id = Uuid::now_v7();
        self.window_number += 1;
        self.baseline = None;
        self.reminder_claimed = false;
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
        assert!(window.claim_reminder());
    }
}
