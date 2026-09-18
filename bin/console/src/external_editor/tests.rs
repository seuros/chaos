use super::*;
use pretty_assertions::assert_eq;
use serial_test::serial;
use tempfile::tempdir;

struct EnvGuard {
    visual: Option<String>,
    editor: Option<String>,
}

impl EnvGuard {
    fn new() -> Self {
        Self {
            visual: env::var("VISUAL").ok(),
            editor: env::var("EDITOR").ok(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_env("VISUAL", self.visual.take());
        restore_env("EDITOR", self.editor.take());
    }
}

fn restore_env(key: &str, value: Option<String>) {
    match value {
        Some(val) => unsafe { env::set_var(key, val) },
        None => unsafe { env::remove_var(key) },
    }
}

#[test]
#[serial]
fn resolve_editor_prefers_visual_and_errors_when_unset() {
    let _guard = EnvGuard::new();
    unsafe {
        env::set_var("VISUAL", "vis");
        env::set_var("EDITOR", "ed");
    }
    let cmd = resolve_editor_command().unwrap();
    assert_eq!(cmd, vec!["vis".to_string()]);

    unsafe {
        env::remove_var("VISUAL");
        env::remove_var("EDITOR");
    }
    assert!(matches!(
        resolve_editor_command(),
        Err(EditorError::MissingEditor)
    ));
}

pub(crate) async fn run_editor_returns_updated_content() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let script_path = dir.path().join("edit.sh");
    fs::write(&script_path, "#!/bin/sh\nprintf \"edited\" > \"$1\"\n").unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let cmd = vec![script_path.to_string_lossy().to_string()];
    let result = run_editor("seed", &cmd).await.unwrap();
    assert_eq!(result, "edited".to_string());
}
