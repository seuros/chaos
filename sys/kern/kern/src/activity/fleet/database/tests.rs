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
