use super::*;
use crate::activity::{Activity, ModelRequest};
use chaos_ipc::ProcessId;

async fn next(rx: &mut mpsc::Receiver<FleetEvent>) -> FleetEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the resident heard nothing")
        .expect("the resident stopped listening")
}

#[tokio::test]
async fn a_resident_hears_a_process_join_speak_and_leave() {
    let home = tempfile::tempdir().unwrap();
    let (listening, mut rx) = listen(home.path()).await.unwrap();
    let socket = runtime_dir(home.path()).join(format!("{}.sock", std::process::id()));
    let origin = Origin {
        kind: OriginKind::Console,
        pid: 4242,
    };
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let event = ActivityEvent {
        process_id: ProcessId::new(),
        at_ms: 7,
        activity: Activity::TurnStarted,
    };
    let mut wire = serde_json::to_vec(&Hello {
        protocol: PROTOCOL,
        origin,
    })
    .unwrap();
    wire.push(b'\n');
    wire.extend(serde_json::to_vec(&event).unwrap());
    wire.push(b'\n');
    stream.write_all(&wire).await.unwrap();

    assert!(matches!(next(&mut rx).await, FleetEvent::Joined(o) if o == origin));
    match next(&mut rx).await {
        FleetEvent::Activity(o, heard) => {
            assert_eq!(o, origin);
            assert_eq!(heard.process_id, event.process_id);
            assert_eq!(heard.activity, Activity::TurnStarted);
        }
        other => panic!("{other:?}"),
    }
    drop(stream);
    assert!(matches!(next(&mut rx).await, FleetEvent::Left(o) if o == origin));

    drop(listening);
    assert!(
        !socket.exists(),
        "a resident removes its socket when it stops"
    );
}

#[tokio::test]
async fn a_process_forwards_its_bus_once_a_resident_listens() {
    let home = tempfile::tempdir().unwrap();
    forward(home.path(), OriginKind::Exec);
    let (_listening, mut rx) = listen(home.path()).await.unwrap();
    let process_id = ProcessId::new();
    let heard = async {
        loop {
            match next(&mut rx).await {
                FleetEvent::Joined(origin) => {
                    assert_eq!(origin.kind, OriginKind::Exec);
                    assert_eq!(origin.pid, std::process::id());
                    // Keep publishing until the forwarder has subscribed.
                    drop(ModelRequest::start(process_id, 1, "model-a"));
                }
                FleetEvent::Activity(_, event) if event.process_id == process_id => {
                    return event.activity;
                }
                FleetEvent::Activity(..) => {
                    drop(ModelRequest::start(process_id, 1, "model-a"));
                }
                FleetEvent::Left(_) => panic!("the forwarder left"),
            }
        }
    };
    let publisher = async {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(ModelRequest::start(process_id, 1, "model-a"));
        }
    };
    let activity = tokio::select! {
        activity = heard => activity,
        () = publisher => unreachable!(),
    };
    assert!(
        matches!(
            activity,
            Activity::ModelStarted { .. } | Activity::ModelEnded { .. }
        ),
        "{activity:?}"
    );
}

#[tokio::test]
async fn an_oversized_line_ends_the_peer_instead_of_growing_the_buffer() {
    let home = tempfile::tempdir().unwrap();
    let (_listening, mut rx) = listen(home.path()).await.unwrap();
    let socket = runtime_dir(home.path()).join(format!("{}.sock", std::process::id()));
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let mut wire = serde_json::to_vec(&Hello {
        protocol: PROTOCOL,
        origin: Origin {
            kind: OriginKind::Mcp,
            pid: 1,
        },
    })
    .unwrap();
    wire.push(b'\n');
    wire.extend(std::iter::repeat_n(b'x', MAX_LINE + 10));
    wire.push(b'\n');
    stream.write_all(&wire).await.unwrap();
    assert!(matches!(next(&mut rx).await, FleetEvent::Joined(_)));
    assert!(matches!(next(&mut rx).await, FleetEvent::Left(_)));
}
