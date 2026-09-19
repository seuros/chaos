use super::*;
use crate::top_bar::tests::{render, text};
use chaos_ipc::ProcessId;

fn working(now: Instant) -> Activity {
    Activity {
        phase: Phase::Working,
        last_activity: Some(now),
        last_runtime_event: Some(now),
        ..Default::default()
    }
}

#[test]
fn only_the_eye_breathes_and_reduced_motion_keeps_quiet_detection() {
    let now = Instant::now();
    let mut snapshot = Snapshot {
        selected: working(now),
        animations: true,
        ..Default::default()
    };
    let (dim, tick) = present(&snapshot, now, Duration::ZERO, 0);
    let (bright, _) = present(&snapshot, now, Duration::from_millis(1400), 0);
    assert!(dim != bright);
    assert_eq!(tick, Some(ANIMATION_TICK));
    assert!(
        present(&snapshot, now, Duration::ZERO, 1).0
            == present(&snapshot, now, Duration::from_millis(1400), 1).0
    );
    snapshot.animations = false;
    assert!(
        present(&snapshot, now, Duration::ZERO, 0).0
            == present(&snapshot, now, Duration::from_millis(1400), 0).0
    );
    assert_eq!(
        present(&snapshot, now, Duration::ZERO, 0).1,
        Some(Duration::from_secs(1))
    );
    let later = now + Duration::from_secs(45);
    assert!(
        present(&snapshot, later, Duration::ZERO, 0).0 == Content::new("◉").tone(Tone::Warning)
    );
    snapshot.selected = Activity::default();
    assert_eq!(present(&snapshot, now, Duration::ZERO, 0).1, None);
}

#[test]
fn busy_agents_do_not_hide_quiet_or_disconnected_siblings() {
    let now = Instant::now();
    let later = now + Duration::from_secs(45);
    let mut snapshot = Snapshot {
        selected: working(later),
        others: vec![(ProcessId::new(), working(now))],
        animations: true,
    };
    assert!(
        present(&snapshot, later, Duration::ZERO, 0).0 == Content::new("◉").tone(Tone::Warning)
    );
    assert!(
        present(&snapshot, later, Duration::ZERO, 2).0
            == Content::new("Others: 1 quiet").tone(Tone::Warning)
    );
    snapshot.others[0].1.phase = Phase::Disconnected;
    assert!(present(&snapshot, later, Duration::ZERO, 0).0 == Content::new("◉").tone(Tone::Error));
}

#[tokio::test]
async fn eye_survives_narrow_layout_and_refreshes_after_hidden_activity() {
    let now = Instant::now();
    let (tx, rx) = watch::channel(Snapshot {
        selected: working(now),
        animations: false,
        ..Default::default()
    });
    let mut widgets = new(rx);
    let wall_time = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    for widget in &mut widgets {
        widget.refresh(&wall_time);
    }
    assert_eq!(text(&render(&widgets, 3)), " ◉ ");
    assert!(text(&render(&widgets, 80)).contains("Working"));
    assert_eq!(text(&render(&widgets, 1)), " ");
    tx.send_modify(|snapshot| snapshot.selected.phase = Phase::NeedsInput);
    for widget in &mut widgets {
        widget.refresh(&wall_time);
    }
    assert!(text(&render(&widgets, 80)).contains("Needs input"));
    assert_eq!(
        render(&widgets, 3)[(1, 0)].fg,
        crate::theme::palette().warning
    );
}
