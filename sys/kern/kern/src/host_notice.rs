//! A single host-owned notice slot per session.
//!
//! A host (for example a hosting shim driving `chaos exec --steer-file`) may
//! want to tell a running turn something time-sensitive, such as "you have not
//! posted for two minutes". Unlike ordinary steered input, such a notice must
//! stay valid until the moment it reaches the model: a newer notice replaces
//! it, the host can withdraw it, and it is dropped if it expires while a long
//! tool call or sample is still running. So it is held here, outside the
//! pending-input queue, and taken only at a delivery boundary (the native
//! sample boundary, or the next clamp tool result), where expiry is checked.
//! A notice is delivered at most once; mirroring the same notice again after
//! delivery does not re-arm it.

use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq)]
pub struct HostNotice {
    /// Identity of this notice; re-setting a delivered id is ignored.
    pub id: String,
    pub text: String,
    /// Unix seconds after which the notice must not be delivered.
    pub expires_at: Option<f64>,
}

#[derive(Debug, Default)]
pub(crate) struct HostNoticeSlot {
    inner: Mutex<SlotState>,
}

#[derive(Debug, Default)]
struct SlotState {
    pending: Option<HostNotice>,
    delivered_id: Option<String>,
}

impl HostNoticeSlot {
    /// Replace (Some) or withdraw (None) the pending notice.
    pub(crate) fn set(&self, notice: Option<HostNotice>) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending =
            notice.filter(|notice| state.delivered_id.as_deref() != Some(notice.id.as_str()));
    }

    /// Take the pending notice for delivery, if it is still valid at `now_unix`.
    pub(crate) fn take(&self, now_unix: f64) -> Option<String> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let notice = state.pending.take()?;
        if notice
            .expires_at
            .is_some_and(|expires_at| expires_at <= now_unix)
        {
            return None;
        }
        state.delivered_id = Some(notice.id);
        Some(notice.text)
    }
}

pub(crate) fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(id: &str, text: &str, expires_at: Option<f64>) -> HostNotice {
        HostNotice {
            id: id.to_string(),
            text: text.to_string(),
            expires_at,
        }
    }

    #[test]
    fn a_notice_that_expires_while_queued_is_never_delivered() {
        let slot = HostNoticeSlot::default();
        slot.set(Some(notice("a", "post a line", Some(100.0))));
        // A long tool call runs past the expiry before the next boundary.
        assert_eq!(slot.take(100.0), None);
        assert_eq!(
            slot.take(50.0),
            None,
            "an expired notice is dropped, not kept"
        );
    }

    #[test]
    fn a_newer_notice_replaces_a_queued_one_instead_of_accumulating() {
        let slot = HostNoticeSlot::default();
        slot.set(Some(notice("a", "two minutes", None)));
        slot.set(Some(notice("b", "five minutes", None)));
        assert_eq!(slot.take(0.0), Some("five minutes".to_string()));
        assert_eq!(slot.take(0.0), None);
    }

    #[test]
    fn withdrawal_removes_a_queued_notice() {
        let slot = HostNoticeSlot::default();
        slot.set(Some(notice("a", "post a line", None)));
        slot.set(None);
        assert_eq!(slot.take(0.0), None);
    }

    #[test]
    fn a_delivered_notice_is_not_rearmed_by_mirroring_it_again() {
        let slot = HostNoticeSlot::default();
        slot.set(Some(notice("a", "post a line", None)));
        assert_eq!(slot.take(0.0), Some("post a line".to_string()));
        slot.set(Some(notice("a", "post a line", None)));
        assert_eq!(slot.take(0.0), None);
        slot.set(Some(notice("b", "still quiet", None)));
        assert_eq!(slot.take(0.0), Some("still quiet".to_string()));
    }
}
