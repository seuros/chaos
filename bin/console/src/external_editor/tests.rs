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
    std::assert_matches!(resolve_editor_command(), Err(EditorError::MissingEditor));
}

#[tokio::test]
async fn run_editor_returns_updated_content() {
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

    assert!(run_editor("seed", &[]).await.is_err());
    assert!(
        run_editor(
            "seed",
            &[dir.path().join("missing").to_string_lossy().into()]
        )
        .await
        .is_err()
    );
    assert!(
        run_editor("seed", &["/bin/sh".into(), "-c".into(), "exit 7".into()])
            .await
            .is_err()
    );

    let marker = dir.path().join("editor-process");
    fs::write(
        &script_path,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$2\" > \"$1\"\nexec /bin/sleep 30\n",
    )
    .unwrap();
    let cmd = vec![
        script_path.to_string_lossy().to_string(),
        marker.to_string_lossy().to_string(),
    ];
    let editor = tokio::spawn(async move { run_editor("cancelled draft", &cmd).await });
    let metadata = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(contents) = fs::read_to_string(&marker)
                && contents.lines().count() == 2
            {
                break contents;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("editor did not start");
    let mut lines = metadata.lines();
    let pid = lines.next().unwrap();
    let draft = std::path::PathBuf::from(lines.next().unwrap());
    assert_eq!(fs::read_to_string(&draft).unwrap(), "cancelled draft");
    editor.abort();
    assert!(editor.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let alive = Command::new("/bin/kill")
                .args(["-0", pid])
                .output()
                .await
                .unwrap()
                .status
                .success();
            if !alive && !draft.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled editor was not reaped and its draft removed");
}
