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
fn daily_advances_to_same_day_if_still_ahead() {
    // 2026-06-01T00:00:00Z, ask for 10:30 the same day.
    let after = Timestamp::from_second(1_780_272_000).expect("ts");
    let sched = Schedule::Daily {
        hour: 10,
        minute: 30,
    };
    let next = sched.next_after(after).expect("next");
    let zdt = Timestamp::from_second(next)
        .expect("ts")
        .to_zoned(TimeZone::UTC);
    assert_eq!(zdt.date().to_string(), "2026-06-01");
    assert_eq!((zdt.hour(), zdt.minute()), (10, 30));
}

#[test]
fn daily_rolls_to_next_day_when_time_has_passed() {
    // 2026-06-01T12:00:00Z, ask for 10:30 (already past today), rolls to Jan 2.
    let after = Timestamp::from_second(1_780_315_200).expect("ts");
    let sched = Schedule::Daily {
        hour: 10,
        minute: 30,
    };
    let next = sched.next_after(after).expect("next");
    let zdt = Timestamp::from_second(next)
        .expect("ts")
        .to_zoned(TimeZone::UTC);
    assert_eq!(zdt.date().to_string(), "2026-06-02");
    assert_eq!((zdt.hour(), zdt.minute()), (10, 30));
}

#[test]
fn weekly_rolls_to_next_matching_weekday() {
    // 2026-06-01 is a Monday. Ask for Wednesday 09:00.
    let after = Timestamp::from_second(1_780_272_000).expect("ts");
    let sched = Schedule::Weekly {
        weekday: Weekday::Wed,
        hour: 9,
        minute: 0,
    };
    let next = sched.next_after(after).expect("next");
    let zdt = Timestamp::from_second(next)
        .expect("ts")
        .to_zoned(TimeZone::UTC);
    assert_eq!(zdt.date().to_string(), "2026-06-03");
    assert_eq!(zdt.weekday(), jiff::civil::Weekday::Wednesday);
    assert_eq!((zdt.hour(), zdt.minute()), (9, 0));
}

#[test]
fn weekly_rolls_a_full_week_when_same_day_but_time_passed() {
    // 2026-06-01T12:00:00Z is Monday noon. Ask for Monday 09:00, already
    // passed today, should roll a full week to 2026-06-08.
    let after = Timestamp::from_second(1_780_315_200).expect("ts");
    let sched = Schedule::Weekly {
        weekday: Weekday::Mon,
        hour: 9,
        minute: 0,
    };
    let next = sched.next_after(after).expect("next");
    let zdt = Timestamp::from_second(next)
        .expect("ts")
        .to_zoned(TimeZone::UTC);
    assert_eq!(zdt.date().to_string(), "2026-06-08");
    assert_eq!((zdt.hour(), zdt.minute()), (9, 0));
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
