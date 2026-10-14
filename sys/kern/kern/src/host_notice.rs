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

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// How many recent ids the slot remembers, so a retried or reordered
/// request for an older notice cannot re-deliver it or displace a newer one.
const REMEMBERED_IDS: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct HostNotice {
    /// Identity of this notice; a delivered id is never delivered again.
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
    /// Recently delivered ids, newest last, bounded.
    delivered: VecDeque<String>,
    /// Recently posted ids (see `post`), newest last, bounded.
    posted: VecDeque<String>,
}

fn remember(ids: &mut VecDeque<String>, id: &str) {
    if ids.iter().any(|seen| seen == id) {
        return;
    }
    if ids.len() == REMEMBERED_IDS {
        ids.pop_front();
    }
    ids.push_back(id.to_string());
}

impl HostNoticeSlot {
    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Make the slot hold exactly what the host's source of truth says now:
    /// `Some` replaces the pending notice (or clears it if that id was already
    /// delivered), `None` withdraws it. Used by the serial `--steer-file`
    /// mirror, where the file is the single, ordered source of truth.
    pub(crate) fn set(&self, notice: Option<HostNotice>) {
        let mut state = self.state();
        state.pending = notice.filter(|notice| !state.delivered.contains(&notice.id));
    }

    /// Offer a notice from a request that may be retried or reordered (HTTP).
    /// An id already posted or delivered is ignored entirely, so a late retry
    /// can neither re-deliver an old notice nor displace a newer one. Returns
    /// whether the notice is now pending.
    pub(crate) fn post(&self, notice: HostNotice) -> bool {
        let mut state = self.state();
        if state.delivered.contains(&notice.id) || state.posted.contains(&notice.id) {
            return false;
        }
        remember(&mut state.posted, &notice.id);
        state.pending = Some(notice);
        true
    }

    /// Withdraw the pending notice. With `Some(id)`, only if that id is the
    /// one pending, so a late withdrawal cannot clear a newer notice. Returns
    /// whether a notice was withdrawn.
    pub(crate) fn withdraw(&self, id: Option<&str>) -> bool {
        let mut state = self.state();
        let matches = match (&state.pending, id) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(pending), Some(id)) => pending.id == id,
        };
        if matches {
            state.pending = None;
        }
        matches
    }

    /// Take the pending notice for delivery, if it is still valid at `now_unix`.
    pub(crate) fn take(&self, now_unix: f64) -> Option<String> {
        let mut state = self.state();
        let notice = state.pending.take()?;
        if notice
            .expires_at
            .is_some_and(|expires_at| expires_at <= now_unix)
        {
            return None;
        }
        remember(&mut state.delivered, &notice.id);
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

    #[test]
    fn a_retried_older_post_neither_redelivers_nor_displaces_a_newer_notice() {
        // Mira's witnesses against 07891a31.
        let slot = HostNoticeSlot::default();
        assert!(slot.post(notice("a", "first", None)));
        assert_eq!(slot.take(0.0), Some("first".to_string()));
        assert!(slot.post(notice("b", "second", None)));
        assert_eq!(slot.take(0.0), Some("second".to_string()));
        assert!(
            !slot.post(notice("a", "first", None)),
            "retry of delivered a"
        );
        assert_eq!(slot.take(0.0), None);

        let slot = HostNoticeSlot::default();
        assert!(slot.post(notice("a", "first", None)));
        assert_eq!(slot.take(0.0), Some("first".to_string()));
        assert!(slot.post(notice("b", "second", None)));
        assert!(
            !slot.post(notice("a", "first", None)),
            "retry of delivered a"
        );
        assert_eq!(slot.take(0.0), Some("second".to_string()), "b survives");

        // An undelivered older id retried after a newer one also loses.
        let slot = HostNoticeSlot::default();
        assert!(slot.post(notice("a", "first", None)));
        assert!(slot.post(notice("b", "second", None)));
        assert!(!slot.post(notice("a", "first", None)));
        assert_eq!(slot.take(0.0), Some("second".to_string()));
    }

    #[test]
    fn a_withdrawal_naming_an_id_only_clears_that_notice() {
        let slot = HostNoticeSlot::default();
        assert!(slot.post(notice("a", "first", None)));
        assert!(slot.post(notice("b", "second", None)));
        assert!(!slot.withdraw(Some("a")), "late withdrawal of a");
        assert_eq!(slot.take(0.0), Some("second".to_string()));
        assert!(slot.post(notice("c", "third", None)));
        assert!(slot.withdraw(Some("c")));
        assert_eq!(slot.take(0.0), None);
    }

    #[test]
    fn remembered_ids_are_bounded() {
        let slot = HostNoticeSlot::default();
        for i in 0..(REMEMBERED_IDS + 5) {
            assert!(slot.post(notice(&i.to_string(), "x", None)));
            assert!(slot.take(0.0).is_some());
        }
        let state = slot.state();
        assert_eq!(state.delivered.len(), REMEMBERED_IDS);
        assert_eq!(state.posted.len(), REMEMBERED_IDS);
    }

    #[test]
    fn the_file_mirror_does_not_redeliver_an_older_delivered_id() {
        let slot = HostNoticeSlot::default();
        slot.set(Some(notice("a", "first", None)));
        assert_eq!(slot.take(0.0), Some("first".to_string()));
        slot.set(Some(notice("b", "second", None)));
        assert_eq!(slot.take(0.0), Some("second".to_string()));
        slot.set(Some(notice("a", "first", None)));
        assert_eq!(slot.take(0.0), None);
    }
}
