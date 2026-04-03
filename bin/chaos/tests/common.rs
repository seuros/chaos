use std::path::Path;

use anyhow::Result;

pub fn chaos_command(chaos_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(chaos_which::cargo_bin("chaos")?);
    cmd.env("CHAOS_HOME", chaos_home);
    Ok(cmd)
}
