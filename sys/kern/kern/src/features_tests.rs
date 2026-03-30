use super::*;

use pretty_assertions::assert_eq;

#[test]
fn under_development_features_are_disabled_by_default() {
    for spec in FEATURES {
        if matches!(spec.stage, Stage::UnderDevelopment) {
            assert_eq!(
                spec.default_enabled, false,
                "feature `{}` is under development and must be disabled by default",
                spec.key
            );
        }
    }
}

#[test]
fn default_enabled_features_are_stable() {
    for spec in FEATURES {
        if spec.default_enabled {
            assert!(
                matches!(spec.stage, Stage::Stable | Stage::Removed),
                "feature `{}` is enabled by default but is not stable/removed ({:?})",
                spec.key,
                spec.stage
            );
        }
    }
}

#[test]
fn use_legacy_landlock_is_removed_and_disabled_by_default() {
    assert_eq!(Feature::UseLegacyLandlock.stage(), Stage::Removed);
    assert_eq!(Feature::UseLegacyLandlock.default_enabled(), false);
}

#[test]
fn use_linux_sandbox_bwrap_is_removed_and_disabled_by_default() {
    assert_eq!(Feature::UseLinuxSandboxBwrap.stage(), Stage::Removed);
    assert_eq!(Feature::UseLinuxSandboxBwrap.default_enabled(), false);
}


#[test]
fn request_permissions_is_under_development() {
    assert_eq!(
        Feature::ExecPermissionApprovals.stage(),
        Stage::UnderDevelopment
    );
    assert_eq!(Feature::ExecPermissionApprovals.default_enabled(), false);
}

#[test]
fn request_permissions_tool_is_under_development() {
    assert_eq!(
        Feature::RequestPermissionsTool.stage(),
        Stage::UnderDevelopment
    );
    assert_eq!(Feature::RequestPermissionsTool.default_enabled(), false);
}

#[test]
fn tool_suggest_is_under_development() {
    assert_eq!(Feature::ToolSuggest.stage(), Stage::UnderDevelopment);
    assert_eq!(Feature::ToolSuggest.default_enabled(), false);
}

#[test]
fn use_linux_sandbox_bwrap_is_a_removed_feature_key() {
    assert_eq!(
        feature_for_key("use_legacy_landlock"),
        Some(Feature::UseLegacyLandlock)
    );
    assert_eq!(
        feature_for_key("use_linux_sandbox_bwrap"),
        Some(Feature::UseLinuxSandboxBwrap)
    );
}

#[test]
fn image_generation_is_under_development() {
    assert_eq!(Feature::ImageGeneration.stage(), Stage::UnderDevelopment);
    assert_eq!(Feature::ImageGeneration.default_enabled(), false);
}

#[test]
fn image_detail_original_feature_is_under_development() {
    assert_eq!(
        Feature::ImageDetailOriginal.stage(),
        Stage::UnderDevelopment
    );
    assert_eq!(Feature::ImageDetailOriginal.default_enabled(), false);
}

#[test]
fn collab_is_legacy_alias_for_multi_agent() {
    assert_eq!(feature_for_key("multi_agent"), Some(Feature::Collab));
    assert_eq!(feature_for_key("collab"), Some(Feature::Collab));
}

#[test]
fn multi_agent_is_stable_and_enabled_by_default() {
    assert_eq!(Feature::Collab.stage(), Stage::Stable);
    assert_eq!(Feature::Collab.default_enabled(), true);
}

#[test]
fn enable_fanout_is_under_development() {
    assert_eq!(Feature::SpawnCsv.stage(), Stage::UnderDevelopment);
    assert_eq!(Feature::SpawnCsv.default_enabled(), false);
}

#[test]
fn enable_fanout_normalization_enables_multi_agent_one_way() {
    let mut enable_fanout_features = Features::with_defaults();
    enable_fanout_features.enable(Feature::SpawnCsv);
    enable_fanout_features.normalize_dependencies();
    assert_eq!(enable_fanout_features.enabled(Feature::SpawnCsv), true);
    assert_eq!(enable_fanout_features.enabled(Feature::Collab), true);

    let mut collab_features = Features::with_defaults();
    collab_features.enable(Feature::Collab);
    collab_features.normalize_dependencies();
    assert_eq!(collab_features.enabled(Feature::Collab), true);
    assert_eq!(collab_features.enabled(Feature::SpawnCsv), false);
}
