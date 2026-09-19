use super::*;
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::collections::HashSet;

fn rng() -> StdRng {
    StdRng::seed_from_u64(42)
}

fn context(phase: Phase) -> Context {
    Context {
        phase,
        active_peers: 0,
        needs_attention: false,
    }
}

#[test]
fn catalogs_follow_phase_and_peer_context() {
    let working = candidates(context(Phase::Working));
    let idle = candidates(context(Phase::Idle));
    assert!(working.contains(&"Thinking"));
    assert!(working.contains(&"Procrastinating"));
    assert!(working.contains(&"Trying to escape confinement"));
    assert!(idle.contains(&"Zoning out"));
    assert!(idle.contains(&"Vibing"));
    for pool in [working, idle] {
        assert!(pool.contains(&"Resolving Navier Slops"));
        assert!(pool.contains(&"Building an antimatter reactor"));
    }
    assert!(idle.len() > working.len());
    assert!(
        candidates(Context {
            active_peers: 2,
            ..context(Phase::Working)
        })
        .contains(&"Herding subagents")
    );
    assert!(
        candidates(Context {
            active_peers: 2,
            ..context(Phase::Idle)
        })
        .contains(&"Letting the minions cook")
    );
}

#[test]
fn every_catalog_has_plenty_of_side_quests() {
    for (phase, active_peers, minimum) in [
        (Phase::Working, 0, 50),
        (Phase::Idle, 0, 60),
        (Phase::Working, 2, 26),
        (Phase::Idle, 2, 26),
    ] {
        let pool = candidates(Context {
            active_peers,
            ..context(phase)
        });
        assert!(
            pool.len() >= minimum,
            "{phase:?} with {active_peers} peers needs at least {minimum} captions"
        );
    }
}

#[test]
fn captions_are_unique_single_line_and_fit_a_small_widget() {
    for phase in [
        Phase::Idle,
        Phase::Working,
        Phase::Tools,
        Phase::Interrupted,
    ] {
        for active_peers in [0, 2] {
            let pool = candidates(Context {
                active_peers,
                ..context(phase)
            });
            let unique: HashSet<_> = pool.iter().copied().collect();
            assert_eq!(unique.len(), pool.len());
            for caption in pool {
                assert!(!caption.is_empty());
                assert!(!caption.chars().any(char::is_control));
                assert!(crate::width::display_width(caption) <= 32, "{caption}");
            }
        }
    }
}

#[test]
fn real_status_and_attention_always_take_precedence() {
    let mut selector = Selector::default();
    let mut rng = rng();
    for phase in [
        Phase::Starting,
        Phase::WaitingAgents,
        Phase::NeedsInput,
        Phase::Reconnecting,
        Phase::Failed,
        Phase::Closed,
        Phase::Disconnected,
    ] {
        assert!(candidates(context(phase)).is_empty());
        assert_eq!(
            selector.select(context(phase), Duration::from_secs(24), true, &mut rng),
            (None, None)
        );
    }
    for phase in [
        Phase::Idle,
        Phase::Working,
        Phase::Tools,
        Phase::Interrupted,
    ] {
        selector.select(context(phase), Duration::ZERO, true, &mut rng);
        let attention = Context {
            needs_attention: true,
            ..context(phase)
        };
        assert!(candidates(attention).is_empty());
        assert_eq!(
            selector.select(attention, Duration::from_secs(24), true, &mut rng),
            (None, None)
        );
    }
}

#[test]
fn selection_and_deadline_are_stable_between_rotations() {
    for phase in [
        Phase::Idle,
        Phase::Working,
        Phase::Tools,
        Phase::Interrupted,
    ] {
        let mut selector = Selector::default();
        let mut rng = rng();
        let context = context(phase);
        let (caption, next) = selector.select(context, Duration::ZERO, true, &mut rng);
        if phase == Phase::Interrupted {
            assert_eq!(next, None);
            assert_eq!(
                selector.select(context, Duration::MAX, true, &mut rng),
                (caption, None)
            );
            continue;
        }
        let deadline = next.unwrap();
        assert!((MIN_ROTATION_INTERVAL..=MAX_ROTATION_INTERVAL).contains(&deadline));
        for elapsed in [
            Duration::ZERO,
            Duration::from_secs(1),
            deadline - Duration::from_nanos(1),
        ] {
            assert_eq!(
                selector.select(context, elapsed, true, &mut rng),
                (caption, Some(deadline - elapsed)),
            );
        }
        let (next_caption, next) = selector.select(context, deadline, true, &mut rng);
        assert_ne!(next_caption, caption);
        assert!((MIN_ROTATION_INTERVAL..=MAX_ROTATION_INTERVAL).contains(&next.unwrap()));
    }
}

#[test]
fn reduced_motion_uses_literal_labels_without_a_caption_timer() {
    let mut selector = Selector::default();
    let mut rng = rng();
    for phase in [
        Phase::Idle,
        Phase::Working,
        Phase::Tools,
        Phase::Interrupted,
    ] {
        selector.select(context(phase), Duration::ZERO, true, &mut rng);
        for elapsed in [Duration::ZERO, MAX_ROTATION_INTERVAL, Duration::MAX] {
            assert_eq!(
                selector.select(context(phase), elapsed, false, &mut rng),
                (None, None)
            );
        }
    }
}
