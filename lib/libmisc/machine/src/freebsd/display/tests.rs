use super::*;

#[test]
fn disconnected_requires_complete_connector_evidence() {
    for values in [vec![], vec![None], vec![Some(3)], vec![Some(2), None]] {
        assert_eq!(connections(values), DisplayState::Unknown);
    }
    assert_eq!(connections([Some(2), Some(2)]), DisplayState::NoneDetected);
    // A connected display is positive evidence even with other unreadable GPUs.
    assert_eq!(connections([None, Some(1)]), DisplayState::Connected);
}

#[test]
fn scan_uses_only_primary_cards_and_does_not_guess_from_missing_devices() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        scan(root.path(), |_| panic!("empty directory")),
        DisplayState::Unknown
    );
    for name in ["renderD128", "card", "card0-HDMI-A-1", "other"] {
        std::fs::write(root.path().join(name), "").unwrap();
    }
    assert_eq!(
        scan(root.path(), |_| panic!("not a primary card")),
        DisplayState::Unknown
    );
    std::fs::write(root.path().join("card0"), "").unwrap();
    std::fs::write(root.path().join("card2"), "").unwrap();
    assert_eq!(
        scan(root.path(), |_| DisplayState::NoneDetected),
        DisplayState::NoneDetected
    );
    assert_eq!(
        scan(root.path(), |path| if path.ends_with("card2") {
            DisplayState::Unknown
        } else {
            DisplayState::NoneDetected
        }),
        DisplayState::Unknown,
    );
    assert_eq!(
        scan(root.path(), |path| if path.ends_with("card2") {
            DisplayState::Unknown
        } else {
            DisplayState::Connected
        }),
        DisplayState::Connected,
    );
    assert_eq!(
        scan(&root.path().join("missing"), |_| panic!("missing")),
        DisplayState::Unknown
    );
}

#[cfg(target_os = "freebsd")]
#[test]
fn drm_queries_bound_arrays_clear_unused_capacities_and_never_force_a_probe() {
    let mut resource_calls = 0;
    let mut connector_calls = 0;
    let state = query_card(
        |resources| {
            resource_calls += 1;
            if resource_calls == 1 {
                assert_eq!(resources.connector_ids, 0);
                resources.count_fbs = 4;
                resources.count_crtcs = 3;
                resources.count_encoders = 2;
                resources.count_connectors = 1;
            } else {
                assert_eq!(resources.count_fbs, 0);
                assert_eq!(resources.count_crtcs, 0);
                assert_eq!(resources.count_encoders, 0);
                assert_eq!(resources.count_connectors, 1);
                // SAFETY: the query owns count_connectors writable u32 entries.
                unsafe { (resources.connector_ids as *mut u32).write(42) };
            }
            Ok(())
        },
        |connector| {
            connector_calls += 1;
            assert_eq!(connector.connector_id, 42);
            assert_eq!(
                connector.count_modes, 1,
                "zero can force a blocking hardware probe"
            );
            assert_ne!(connector.modes, 0);
            assert_eq!(connector.count_properties, 0);
            assert_eq!(connector.count_encoders, 0);
            connector.connection = 1;
            Ok(())
        },
    );
    assert_eq!(state, DisplayState::Connected);
    assert_eq!((resource_calls, connector_calls), (2, 1));
}

#[cfg(target_os = "freebsd")]
#[test]
fn drm_hotplug_growth_and_invalid_counts_never_read_unfilled_ids() {
    let mut calls = 0;
    let state = query_card(
        |resources| {
            calls += 1;
            resources.count_connectors = if resources.connector_ids == 0 { 1 } else { 2 };
            Ok(())
        },
        |_| panic!("hotplug left no complete connector array"),
    );
    assert_eq!(state, DisplayState::Unknown);
    assert_eq!(calls, 6, "retry budget is finite");
    for count in [0, 4097, u32::MAX] {
        assert_eq!(
            query_card(
                |resources| {
                    resources.count_connectors = count;
                    Ok(())
                },
                |_| panic!("invalid inventory")
            ),
            DisplayState::Unknown,
        );
    }
    assert_eq!(
        query_card(
            |_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            |_| panic!("unavailable GPU"),
        ),
        DisplayState::Unknown,
    );
}
