#![expect(clippy::expect_used)]

use chaos_which::CargoBinError;
use ctor::ctor;
use tempfile::TempDir;

use chaos_kern::Process;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_realpath::AbsolutePathBuf;
use regex_lite::Regex;
use std::path::PathBuf;

pub mod apps_test_server;
pub mod context_snapshot;
pub mod process;
pub mod responses;
pub mod streaming_sse;
pub mod test_chaos_fork;
pub mod test_codex;
pub mod ws_accept;

#[ctor]
fn enable_deterministic_unified_exec_process_ids_for_tests() {
    chaos_kern::test_support::set_process_table_test_mode(/*enabled*/ true);
    chaos_kern::test_support::set_deterministic_process_ids(/*enabled*/ true);
}

#[ctor]
fn configure_insta_workspace_root_for_snapshot_tests() {
    if std::env::var_os("INSTA_WORKSPACE_ROOT").is_some() {
        return;
    }

    let workspace_root = chaos_which::repo_root().ok().map(|root| root.join("chaos"));

    if let Some(workspace_root) = workspace_root
        && let Ok(workspace_root) = workspace_root.canonicalize()
    {
        // Safety: this ctor runs at process startup before test threads begin.
        unsafe {
            std::env::set_var("INSTA_WORKSPACE_ROOT", workspace_root);
        }
    }
}

#[track_caller]
pub fn assert_regex_match<'s>(pattern: &str, actual: &'s str) -> regex_lite::Captures<'s> {
    let regex = Regex::new(pattern).unwrap_or_else(|err| {
        panic!("failed to compile regex {pattern:?}: {err}");
    });
    regex
        .captures(actual)
        .unwrap_or_else(|| panic!("regex {pattern:?} did not match {actual:?}"))
}

pub fn test_path_buf_with_windows(unix_path: &str, _windows_path: Option<&str>) -> PathBuf {
    PathBuf::from(unix_path)
}

pub fn test_path_buf(unix_path: &str) -> PathBuf {
    test_path_buf_with_windows(unix_path, /*windows_path*/ None)
}

pub fn test_absolute_path_with_windows(
    unix_path: &str,
    windows_path: Option<&str>,
) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(test_path_buf_with_windows(unix_path, windows_path))
        .expect("test path should be absolute")
}

pub fn test_absolute_path(unix_path: &str) -> AbsolutePathBuf {
    test_absolute_path_with_windows(unix_path, /*windows_path*/ None)
}

pub fn test_tmp_path() -> AbsolutePathBuf {
    test_absolute_path_with_windows("/tmp", Some(r"C:\Users\codex\AppData\Local\Temp"))
}

pub fn test_tmp_path_buf() -> PathBuf {
    test_tmp_path().into_path_buf()
}

/// Returns a default `Config` whose on-disk state is confined to the provided
/// temporary directory. Using a per-test directory keeps tests hermetic and
/// avoids clobbering a developer’s real `~/.codex`.
pub async fn load_default_config_for_test(chaos_home: &TempDir) -> Config {
    ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(default_test_overrides())
        .build()
        .await
        .expect("defaults for test should always succeed")
}

#[cfg(target_os = "linux")]
fn default_test_overrides() -> ConfigOverrides {
    ConfigOverrides {
        alcatraz_linux_exe: Some(
            chaos_which::cargo_bin("alcatraz-linux")
                .expect("should find binary for alcatraz-linux"),
        ),
        ..ConfigOverrides::default()
    }
}

#[cfg(target_os = "macos")]
fn default_test_overrides() -> ConfigOverrides {
    ConfigOverrides {
        alcatraz_macos_exe: Some(
            chaos_which::cargo_bin("alcatraz-macos")
                .expect("should find binary for alcatraz-macos"),
        ),
        ..ConfigOverrides::default()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn default_test_overrides() -> ConfigOverrides {
    ConfigOverrides::default()
}

/// Builds an SSE stream body from a JSON fixture.
///
/// The fixture must contain an array of objects where each object represents a
/// single SSE event with at least a `type` field matching the `event:` value.
/// Additional fields become the JSON payload for the `data:` line. An object
/// with only a `type` field results in an event with no `data:` section. This
/// makes it trivial to extend the fixtures as OpenAI adds new event kinds or
/// fields.
pub fn load_sse_fixture(path: impl AsRef<std::path::Path>) -> String {
    let events: Vec<serde_json::Value> =
        serde_json::from_reader(std::fs::File::open(path).expect("read fixture"))
            .expect("parse JSON fixture");
    events
        .into_iter()
        .map(|e| {
            let kind = e
                .get("type")
                .and_then(|v| v.as_str())
                .expect("fixture event missing type");
            if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
                format!("event: {kind}\n\n")
            } else {
                format!("event: {kind}\ndata: {e}\n\n")
            }
        })
        .collect()
}

pub fn load_sse_fixture_with_id_from_str(raw: &str, id: &str) -> String {
    let replaced = raw.replace("__ID__", id);
    let events: Vec<serde_json::Value> =
        serde_json::from_str(&replaced).expect("parse JSON fixture");
    events
        .into_iter()
        .map(|e| {
            let kind = e
                .get("type")
                .and_then(|v| v.as_str())
                .expect("fixture event missing type");
            if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
                format!("event: {kind}\n\n")
            } else {
                format!("event: {kind}\ndata: {e}\n\n")
            }
        })
        .collect()
}

pub async fn wait_for_event<F>(codex: &Process, predicate: F) -> chaos_ipc::protocol::EventMsg
where
    F: FnMut(&chaos_ipc::protocol::EventMsg) -> bool,
{
    use tokio::time::Duration;
    wait_for_event_with_timeout(codex, predicate, Duration::from_secs(1)).await
}

pub async fn wait_for_event_match<T, F>(codex: &Process, matcher: F) -> T
where
    F: Fn(&chaos_ipc::protocol::EventMsg) -> Option<T>,
{
    let ev = wait_for_event(codex, |ev| matcher(ev).is_some()).await;
    matcher(&ev).unwrap()
}

pub async fn wait_for_event_with_timeout<F>(
    codex: &Process,
    mut predicate: F,
    wait_time: tokio::time::Duration,
) -> chaos_ipc::protocol::EventMsg
where
    F: FnMut(&chaos_ipc::protocol::EventMsg) -> bool,
{
    use tokio::time::Duration;
    use tokio::time::timeout;
    loop {
        // Allow a bit more time to accommodate async startup work (e.g. config IO, tool discovery)
        let ev = timeout(wait_time.max(Duration::from_secs(10)), codex.next_event())
            .await
            .expect("timeout waiting for event")
            .expect("stream ended unexpectedly");
        if predicate(&ev.msg) {
            return ev.msg;
        }
    }
}

pub fn sandbox_env_var() -> &'static str {
    chaos_kern::spawn::CHAOS_SANDBOX_ENV_VAR
}

pub fn sandbox_network_env_var() -> &'static str {
    chaos_kern::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR
}

pub fn format_with_current_shell(command: &str) -> Vec<String> {
    chaos_kern::shell::default_user_shell().derive_exec_args(command, /*use_login_shell*/ true)
}

pub fn format_with_current_shell_display(command: &str) -> String {
    let args = format_with_current_shell(command);
    shlex::try_join(args.iter().map(String::as_str)).expect("serialize current shell command")
}

pub fn format_with_current_shell_non_login(command: &str) -> Vec<String> {
    chaos_kern::shell::default_user_shell()
        .derive_exec_args(command, /*use_login_shell*/ false)
}

pub fn format_with_current_shell_display_non_login(command: &str) -> String {
    let args = format_with_current_shell_non_login(command);
    shlex::try_join(args.iter().map(String::as_str))
        .expect("serialize current shell command without login")
}

pub fn stdio_server_bin() -> Result<String, CargoBinError> {
    chaos_which::cargo_bin("test_stdio_server").map(|p| p.to_string_lossy().to_string())
}

pub mod fs_wait {
    use anyhow::Result;
    use anyhow::anyhow;
    use notify::RecursiveMode;
    use notify::Watcher;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    use std::time::Instant;
    use tokio::task;
    use walkdir::WalkDir;

    pub async fn wait_for_path_exists(
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<PathBuf> {
        let path = path.into();
        task::spawn_blocking(move || wait_for_path_exists_blocking(path, timeout)).await?
    }

    pub async fn wait_for_matching_file(
        root: impl Into<PathBuf>,
        timeout: Duration,
        predicate: impl FnMut(&Path) -> bool + Send + 'static,
    ) -> Result<PathBuf> {
        let root = root.into();
        task::spawn_blocking(move || {
            let mut predicate = predicate;
            blocking_find_matching_file(root, timeout, &mut predicate)
        })
        .await?
    }

    fn wait_for_path_exists_blocking(path: PathBuf, timeout: Duration) -> Result<PathBuf> {
        if path.exists() {
            return Ok(path);
        }

        let watch_root = nearest_existing_ancestor(&path);
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        watcher.watch(&watch_root, RecursiveMode::Recursive)?;

        let deadline = Instant::now() + timeout;
        loop {
            if path.exists() {
                return Ok(path.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match rx.recv_timeout(remaining) {
                Ok(Ok(_event)) => {
                    if path.exists() {
                        return Ok(path.clone());
                    }
                }
                Ok(Err(err)) => return Err(err.into()),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if path.exists() {
            Ok(path)
        } else {
            Err(anyhow!("timed out waiting for {path:?}"))
        }
    }

    fn blocking_find_matching_file(
        root: PathBuf,
        timeout: Duration,
        predicate: &mut impl FnMut(&Path) -> bool,
    ) -> Result<PathBuf> {
        let root = wait_for_path_exists_blocking(root, timeout)?;

        if let Some(found) = scan_for_match(&root, predicate) {
            return Ok(found);
        }

        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;

        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(_event)) => {
                    if let Some(found) = scan_for_match(&root, predicate) {
                        return Ok(found);
                    }
                }
                Ok(Err(err)) => return Err(err.into()),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Some(found) = scan_for_match(&root, predicate) {
            Ok(found)
        } else {
            Err(anyhow!("timed out waiting for matching file in {root:?}"))
        }
    }

    fn scan_for_match(root: &Path, predicate: &mut impl FnMut(&Path) -> bool) -> Option<PathBuf> {
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            if predicate(path) {
                return Some(path.to_path_buf());
            }
        }
        None
    }

    fn nearest_existing_ancestor(path: &Path) -> PathBuf {
        let mut current = path;
        loop {
            if current.exists() {
                return current.to_path_buf();
            }
            match current.parent() {
                Some(parent) => current = parent,
                None => return PathBuf::from("."),
            }
        }
    }
}

#[macro_export]
macro_rules! skip_if_sandbox {
    () => {{
        if ::std::env::var($crate::sandbox_env_var())
            == ::core::result::Result::Ok("seatbelt".to_string())
        {
            eprintln!(
                "{} is set to 'seatbelt', skipping test.",
                $crate::sandbox_env_var()
            );
            return;
        }
    }};
    ($return_value:expr $(,)?) => {{
        if ::std::env::var($crate::sandbox_env_var())
            == ::core::result::Result::Ok("seatbelt".to_string())
        {
            eprintln!(
                "{} is set to 'seatbelt', skipping test.",
                $crate::sandbox_env_var()
            );
            return $return_value;
        }
    }};
}

#[macro_export]
macro_rules! skip_if_no_network {
    () => {{
        if ::std::env::var($crate::sandbox_network_env_var()).is_ok() {
            println!(
                "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
            );
            return;
        }
    }};
    ($return_value:expr $(,)?) => {{
        if ::std::env::var($crate::sandbox_network_env_var()).is_ok() {
            println!(
                "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
            );
            return $return_value;
        }
    }};
}

#[macro_export]
macro_rules! alcatraz_linux_exe_or_skip {
    () => {{
        #[cfg(target_os = "linux")]
        {
            match chaos_which::cargo_bin("alcatraz-linux") {
                Ok(path) => Some(path),
                Err(err) => {
                    eprintln!("alcatraz-linux binary not available, skipping test: {err}");
                    return;
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }};
    ($return_value:expr $(,)?) => {{
        #[cfg(target_os = "linux")]
        {
            match chaos_which::cargo_bin("alcatraz-linux") {
                Ok(path) => Some(path),
                Err(err) => {
                    eprintln!("alcatraz-linux binary not available, skipping test: {err}");
                    return $return_value;
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }};
}

/// No-op: Windows is not a supported target.
#[macro_export]
macro_rules! skip_if_windows {
    ($return_value:expr $(,)?) => {{}};
}
