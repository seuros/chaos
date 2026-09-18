use super::*;
use std::fs;

#[test]
fn discovery_suite() {
    discovers_project_scripts();
    missing_dir_returns_empty();
}

fn discovers_project_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    let scripts_dir = tmp.path().join(".chaos").join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();

    fs::write(scripts_dir.join("b_second.lua"), "-- second").unwrap();
    fs::write(scripts_dir.join("a_first.lua"), "-- first").unwrap();
    fs::write(scripts_dir.join("not_lua.txt"), "-- ignored").unwrap();

    // Pass a non-existent path as the user-layer override so real user
    // scripts (e.g. ~/.config/chaos/scripts/project_info.lua) are never
    // loaded.
    let found = discover_scripts(tmp.path(), Some(&tmp.path().join("no_user_scripts")));
    let names: Vec<&str> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();

    assert_eq!(names, vec!["a_first.lua", "b_second.lua"]);
}

fn missing_dir_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let found = discover_scripts(tmp.path(), Some(&tmp.path().join("no_user_scripts")));
    assert!(found.is_empty());
}
