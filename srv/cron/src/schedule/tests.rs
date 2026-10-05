use super::*;

#[test]
fn interval_round_trips_and_advances() {
    let sched = Schedule::Interval { seconds: 300 };
    let json = sched.to_json();
    let parsed = Schedule::parse(&json).expect("parse");
    let ts = Timestamp::from_second(1000).expect("ts");
    assert_eq!(parsed.next_after(ts).expect("next"), 1300);
}

#[test]
fn interval_rejects_non_positive_seconds() {
    let sched = Schedule::Interval { seconds: 0 };
    let json = sched.to_json();
    assert!(Schedule::parse(&json).is_err());
}

#[test]
fn daily_advances_to_the_next_scheduled_time() {
    let sched = Schedule::Daily {
        hour: 10,
        minute: 30,
    };
    for (label, after, expected) in [
        (
            "same day, still ahead",
            1_780_272_000,
            "2026-06-01T10:30:00Z",
        ),
        (
            "next day, time passed",
            1_780_315_200,
            "2026-06-02T10:30:00Z",
        ),
    ] {
        let after = Timestamp::from_second(after).expect(label);
        let expected: Timestamp = expected.parse().expect(label);
        assert_eq!(
            sched.next_after(after).expect(label),
            expected.as_second(),
            "{label}"
        );
    }
}

#[test]
fn weekly_advances_to_the_next_matching_weekday() {
    for (label, weekday, after, expected) in [
        (
            "later weekday",
            Weekday::Wed,
            1_780_272_000,
            "2026-06-03T09:00:00Z",
        ),
        (
            "full week, same day but time passed",
            Weekday::Mon,
            1_780_315_200,
            "2026-06-08T09:00:00Z",
        ),
    ] {
        let sched = Schedule::Weekly {
            weekday,
            hour: 9,
            minute: 0,
        };
        let after = Timestamp::from_second(after).expect(label);
        let expected: Timestamp = expected.parse().expect(label);
        assert_eq!(
            sched.next_after(after).expect(label),
            expected.as_second(),
            "{label}"
        );
    }
}

#[test]
fn invalid_hour_is_rejected() {
    let sched = Schedule::Daily {
        hour: 24,
        minute: 0,
    };
    let json = sched.to_json();
    assert!(Schedule::parse(&json).is_err());
}
