use super::*;
use crate::activity::ModelRequest;
use crate::activity::fleet::OriginKind;
use chaos_ipc::ProcessId;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_resident_hears_another_machine_through_the_database() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let mut heard = listen(&url).await.unwrap();
    let pool = PgPool::connect(&url).await.unwrap();
    let origin = Origin {
        kind: OriginKind::Console,
        pid: 4242,
        host: "silica".into(),
    };
    tokio::spawn(forward(pool, origin.clone()));
    let process_id = ProcessId::new();
    let found = async {
        loop {
            match heard.recv().await.unwrap() {
                FleetEvent::Joined(joined) => assert_eq!(joined, origin),
                FleetEvent::Activity(from, event) if event.process_id == process_id => {
                    assert_eq!(from, origin);
                    return event;
                }
                _ => drop(ModelRequest::start(process_id, 1, "model-a")),
            }
        }
    };
    let publisher = async {
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(ModelRequest::start(process_id, 1, "model-a"));
        }
    };
    let event = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            event = found => event,
            () = publisher => unreachable!(),
        }
    })
    .await
    .expect("nothing crossed the database");
    assert_eq!(event.process_id, process_id);
}

#[test]
fn only_postgres_storage_is_shared() {
    assert!(shared("postgres://u:p@harbor:5432/chaos"));
    assert!(shared("postgresql://harbor/chaos"));
    assert!(!shared("sqlite:///home/me/.chaos/chaos.sqlite"));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_resident_keeps_listening_after_the_server_drops_it() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let mut heard = listen(&url).await.unwrap();
    let pool = PgPool::connect(&url).await.unwrap();
    // Kill the resident's listening backend from the server side.
    let killed: i64 = sqlx::query_scalar(
        "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity
         WHERE query ILIKE 'LISTEN %chaos_activity%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(killed, 1);
    let origin = Origin {
        kind: OriginKind::Exec,
        pid: 1,
        host: "silica".into(),
    };
    let heard_again = async {
        loop {
            notify(
                &pool,
                &Notice {
                    origin: origin.clone(),
                    event: None,
                },
            )
            .await;
            if let Ok(Some(FleetEvent::Joined(joined))) =
                tokio::time::timeout(Duration::from_millis(500), heard.recv()).await
            {
                return joined;
            }
        }
    };
    let joined = tokio::time::timeout(Duration::from_secs(20), heard_again)
        .await
        .expect("the resident never listened again");
    assert_eq!(joined, origin);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_probed_resident_still_looks_like_it_is_listening() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let _heard = listen(&url).await.unwrap();
    let pool = PgPool::connect(&url).await.unwrap();
    // Several probes run in this time under test.
    tokio::time::sleep(PROBE * 4).await;
    assert!(
        watched(&pool).await,
        "processes stop forwarding when the probe hides the LISTEN"
    );
}

fn peer(pid: u32) -> Origin {
    Origin {
        kind: OriginKind::Exec,
        pid,
        host: "silica".into(),
    }
}

fn at(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

#[test]
fn a_peer_that_falls_silent_leaves() {
    let mut presence = Presence::new();
    let origin = peer(2);
    for beat in 0..=10 {
        presence.heard(&origin, true, at(beat * 10));
    }
    assert_eq!(presence.sweep(at(140)), vec![origin.clone()]);
    assert!(presence.sweep(at(150)).is_empty());
    assert_eq!(presence.heard(&origin, true, at(160)), Heard::Joined);
}

#[test]
fn a_peer_heard_once_leaves_after_the_silence() {
    let mut presence = Presence::new();
    let origin = peer(3);
    assert_eq!(presence.heard(&origin, true, at(0)), Heard::Joined);
    assert!(presence.sweep(at(30)).is_empty());
    assert_eq!(presence.sweep(at(31)), vec![origin]);
}

#[test]
fn activity_alone_neither_times_nor_keeps_a_peer() {
    let mut presence = Presence::new();
    let origin = peer(4);
    assert_eq!(presence.heard(&origin, false, at(0)), Heard::Joined);
    for second in 1..=20 {
        assert_eq!(presence.heard(&origin, false, at(second)), Heard::Present);
    }
    assert_eq!(presence.sweep(at(31)), vec![origin]);
}

#[test]
fn a_full_roster_leaves_new_peers_unheard() {
    let mut presence = Presence::new();
    for pid in 0..u32::try_from(PEERS).unwrap() {
        assert_eq!(presence.heard(&peer(pid), true, at(0)), Heard::Joined);
    }
    let late = peer(u32::MAX);
    assert_eq!(presence.heard(&late, true, at(1)), Heard::Unheard);
    assert_eq!(presence.sweep(at(31)).len(), PEERS);
    assert_eq!(presence.heard(&late, true, at(32)), Heard::Joined);
}
