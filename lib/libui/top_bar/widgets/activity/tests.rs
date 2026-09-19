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
    let mut captions = captions::Selector::default();
    let now = Instant::now();
    let mut snapshot = Snapshot {
        selected: working(now),
        animations: true,
        ..Default::default()
    };
    let beat = Duration::from_millis(1400);
    let (dim, tick) = present(&snapshot, now, Duration::ZERO, 0, &mut captions);
    let (bright, _) = present(&snapshot, now, beat, 0, &mut captions);
    assert!(dim != bright);
    assert_eq!(tick, Some(ANIMATION_TICK));
    assert!(
        present(&snapshot, now, Duration::ZERO, 1, &mut captions).0
            == present(&snapshot, now, beat, 1, &mut captions).0
    );
    snapshot.animations = false;
    assert!(
        present(&snapshot, now, Duration::ZERO, 0, &mut captions).0
            == present(&snapshot, now, beat, 0, &mut captions).0
    );
    assert_eq!(
        present(&snapshot, now, Duration::ZERO, 0, &mut captions).1,
        Some(Duration::from_secs(1))
    );
    let later = now + Duration::from_secs(45);
    assert!(
        present(&snapshot, later, Duration::ZERO, 0, &mut captions).0
            == Content::new("◉").tone(Tone::Warning)
    );
    snapshot.selected = Activity::default();
    assert_eq!(
        present(&snapshot, now, Duration::ZERO, 0, &mut captions).1,
        None
    );
}

#[test]
fn busy_agents_do_not_hide_quiet_or_disconnected_siblings() {
    let mut captions = captions::Selector::default();
    let now = Instant::now();
    let later = now + Duration::from_secs(45);
    let mut snapshot = Snapshot {
        selected: working(later),
        others: vec![(ProcessId::new(), working(now))],
        animations: true,
    };
    assert!(
        present(&snapshot, later, Duration::ZERO, 0, &mut captions).0
            == Content::new("◉").tone(Tone::Warning)
    );
    assert!(
        present(&snapshot, later, Duration::ZERO, 2, &mut captions).0
            == Content::new("Others: 1 quiet").tone(Tone::Warning)
    );
    assert!(
        present(&snapshot, later, Duration::from_secs(10), 1, &mut captions).0
            == Content::new("Working").tone(Tone::Accent),
        "a sibling warning suppresses playful captions"
    );
    snapshot.others[0].1.phase = Phase::Disconnected;
    assert!(
        present(&snapshot, later, Duration::ZERO, 0, &mut captions).0
            == Content::new("◉").tone(Tone::Error)
    );
}

#[test]
fn playful_text_rotates_without_renewing_activity_or_hiding_quiet() {
    let mut captions = captions::Selector::default();
    let now = Instant::now();
    let snapshot = Snapshot {
        selected: working(now),
        animations: true,
        ..Default::default()
    };
    let initial = present(&snapshot, now, Duration::ZERO, 1, &mut captions).0;
    let elapsed = Duration::from_secs(9);
    assert!(present(&snapshot, now + elapsed, elapsed, 1, &mut captions).0 == initial);
    let elapsed = Duration::from_secs(35);
    assert!(
        present(&snapshot, now + elapsed, elapsed, 1, &mut captions).0
            == Content::new("No activity · 35s").tone(Tone::Warning)
    );
    assert_eq!(snapshot.selected.last_activity, Some(now));
    assert_eq!(snapshot.selected.last_runtime_event, Some(now));

    let idle = Snapshot {
        animations: true,
        ..Default::default()
    };
    let (label, next) = present(&idle, now, Duration::ZERO, 1, &mut captions);
    let elapsed = next.unwrap();
    assert!((captions::MIN_ROTATION_INTERVAL..=captions::MAX_ROTATION_INTERVAL).contains(&elapsed));
    let (rotated, next) = present(&idle, now + elapsed, elapsed, 1, &mut captions);
    assert!(rotated != label);
    assert!(
        (captions::MIN_ROTATION_INTERVAL..=captions::MAX_ROTATION_INTERVAL)
            .contains(&next.unwrap())
    );
    assert!(
        present(&idle, now, Duration::ZERO, 0, &mut captions).0
            == present(&idle, now + elapsed, elapsed, 0, &mut captions).0,
        "idle captions do not make the eye breathe"
    );
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
