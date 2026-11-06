use super::*;

fn key(session: &str, id: &str) -> (SessionId, ElicitationId) {
    (SessionId::new(session), ElicitationId::new(id))
}

fn reserve(ledger: &mut UrlLedger, session: &str, id: &str) -> Option<ClaimId> {
    let (session, id) = key(session, id);
    ledger.reserve(session, id, |_| false)
}

#[test]
fn duplicate_url_ids_are_refused_until_released_or_completed() {
    let mut ledger = UrlLedger::with_capacity(4);
    let first = reserve(&mut ledger, "s", "e").unwrap();
    assert!(reserve(&mut ledger, "s", "e").is_none());
    assert!(ledger.retain(first));
    assert!(reserve(&mut ledger, "s", "e").is_none());
    assert!(!ledger.release(first));
    assert!(ledger.complete(&SessionId::new("s"), &ElicitationId::new("e")));
    assert!(!ledger.complete(&SessionId::new("s"), &ElicitationId::new("e")));
    let again = reserve(&mut ledger, "s", "e").unwrap();
    assert_ne!(first, again);
    assert!(ledger.release(again));
    assert!(reserve(&mut ledger, "s", "e").is_some());
}

#[test]
fn stale_claims_cannot_retain_a_replacement_reservation() {
    let mut ledger = UrlLedger::with_capacity(4);
    let stale = reserve(&mut ledger, "s", "e").unwrap();
    assert!(ledger.release(stale));
    let live = reserve(&mut ledger, "s", "e").unwrap();
    assert!(!ledger.retain(stale));
    assert!(!ledger.release(stale));
    assert!(ledger.retain(live));
}

#[test]
fn completion_is_session_scoped_and_dead_sessions_free_their_ids() {
    let mut ledger = UrlLedger::with_capacity(2);
    let (session, id) = key("s", "e");
    let claim = ledger.reserve(session, id, |_| false).unwrap();
    assert!(ledger.retain(claim));
    assert!(!ledger.complete(&SessionId::new("other"), &ElicitationId::new("e")));
    assert!(reserve(&mut ledger, "other", "x").is_some());
    assert!(
        reserve(&mut ledger, "other", "y").is_none(),
        "budget is exhausted"
    );
    let (session, id) = key("other", "y");
    assert!(
        ledger
            .reserve(session, id, |owner| owner.0.as_ref() == "s")
            .is_some()
    );
    assert!(!ledger.complete(&SessionId::new("s"), &ElicitationId::new("e")));
}

#[tokio::test(start_paused = true)]
async fn outstanding_url_ids_expire_after_the_ttl() {
    let mut ledger = UrlLedger::with_capacity(4);
    let claim = reserve(&mut ledger, "s", "e").unwrap();
    assert!(ledger.retain(claim));
    tokio::time::advance(URL_ELICITATION_TTL - Duration::from_secs(1)).await;
    assert!(reserve(&mut ledger, "s", "e").is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(reserve(&mut ledger, "s", "e").is_some());
}
