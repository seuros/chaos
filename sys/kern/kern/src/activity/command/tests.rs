use super::*;
use pretty_assertions::assert_eq;

fn classify_script(script: &str) -> (Station, String) {
    let command = classify(&["bash".to_string(), "-lc".to_string(), script.to_string()]);
    (command.station, command.label)
}

#[test]
fn shell_scripts_are_judged_by_every_program_they_run() {
    let cases = [
        (
            "cd /repo && cat src/main.rs | grep fn",
            Station::Files,
            "cat",
        ),
        (
            "cd /repo && cargo test 2>&1 | tail -n 20",
            Station::Shell,
            "cargo",
        ),
        (
            "echo \"$(curl -s https://example.test)\"",
            Station::Network,
            "curl",
        ),
        ("git -C /repo push origin main", Station::Network, "git"),
        ("git -C /repo status", Station::Shell, "git"),
        (
            "RUST_LOG=debug sudo -u build timeout 30 cargo publish",
            Station::Network,
            "cargo",
        ),
        (
            r"find . -name '*.orig' -exec rm {} \;",
            Station::Files,
            "find",
        ),
        (
            "python3 -m pip install requests",
            Station::Network,
            "python3",
        ),
        ("bun install", Station::Network, "bun"),
        ("bun run build", Station::Shell, "bun"),
        (
            "podman --connection remote pull quay.io/x",
            Station::Network,
            "podman",
        ),
        ("upkg --self-upgrade", Station::Network, "upkg"),
        ("upkg list", Station::Shell, "upkg"),
        (
            "blackship -c jails.toml bootstrap 15.1-RELEASE",
            Station::Network,
            "blackship",
        ),
        ("blackship up --all", Station::Shell, "blackship"),
        ("cd /repo && pwd", Station::Shell, "cd"),
    ];
    for (script, station, label) in cases {
        assert_eq!(
            classify_script(script),
            (station, label.to_string()),
            "{script}"
        );
    }
}

#[test]
fn direct_commands_are_named_without_their_path() {
    let argv = ["/usr/bin/CURL.EXE", "-H", "Authorization: x"].map(str::to_string);
    let command = classify(&argv);
    assert_eq!(
        (command.station, command.label),
        (Station::Network, "curl".to_string())
    );
}
