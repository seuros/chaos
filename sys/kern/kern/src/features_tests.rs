use super::*;

use pretty_assertions::assert_eq;

#[test]
fn spawn_csv_is_disabled_by_default() {
    assert_eq!(Feature::SpawnCsv.default_enabled(), false);
}
