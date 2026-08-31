//! Shell snapshot actor.

use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::shell::Shell;
use crate::shell::ShellType;
use crate::shell::get_shell;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use chaos_ipc::ProcessId;
use chaos_journald::JournalStore;
use chaos_journald::SqliteJournalStore;
use chaos_snitch::SessionTelemetry;
use chaos_traits::router::Adapter;
use chaos_traits::router::AdapterError;
use chaos_traits::router::DEFAULT_ADAPTER_CAPACITY;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing::Instrument;
use tracing::info_span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSnapshot {
    pub path: PathBuf,
    pub cwd: PathBuf,
}

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const SNAPSHOT_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 3);
const SNAPSHOT_DIR: &str = "shell_snapshots";
const EXCLUDED_EXPORT_VARS: &[&str] = &["PWD", "OLDPWD"];

enum ShellSnapshotOp {
    Refresh { cwd: PathBuf },
}

pub(crate) enum ShellSnapshotStartup {
    Inherited(Arc<ShellSnapshot>),
    Capture(PathBuf),
    #[cfg(test)]
    Idle,
}

#[derive(Clone)]
pub(crate) struct ShellSnapshotActor {
    mailbox: Adapter<ShellSnapshotOp, ()>,
}

impl ShellSnapshotActor {
    pub(crate) fn spawn(
        chaos_home: PathBuf,
        session_id: ProcessId,
        startup: ShellSnapshotStartup,
        shell: &mut Shell,
        session_telemetry: SessionTelemetry,
    ) -> Self {
        Self::spawn_inner(
            chaos_home,
            session_id,
            startup,
            shell,
            Some(session_telemetry),
        )
    }

    fn spawn_inner(
        chaos_home: PathBuf,
        session_id: ProcessId,
        startup: ShellSnapshotStartup,
        shell: &mut Shell,
        session_telemetry: Option<SessionTelemetry>,
    ) -> Self {
        let (initial_snapshot, initial_cwd) = match startup {
            ShellSnapshotStartup::Inherited(snapshot) => (Some(snapshot), None),
            ShellSnapshotStartup::Capture(cwd) => (None, Some(cwd)),
            #[cfg(test)]
            ShellSnapshotStartup::Idle => (None, None),
        };

        let (shell_snapshot_tx, shell_snapshot_rx) = watch::channel(initial_snapshot);
        shell.shell_snapshot = shell_snapshot_rx;
        let snapshot_shell = shell.clone();
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);

        tokio::spawn(async move {
            if let Some(initial_cwd) = initial_cwd {
                create_and_publish_snapshot(
                    &chaos_home,
                    session_id,
                    initial_cwd.as_path(),
                    &snapshot_shell,
                    &shell_snapshot_tx,
                    session_telemetry.as_ref(),
                )
                .await;
            }

            if let Err(err) = cleanup_stale_snapshots(&chaos_home, session_id).await {
                tracing::warn!("Failed to clean up shell snapshots: {err:?}");
            }

            while let Some(packet) = receiver.recv().await {
                let ShellSnapshotOp::Refresh { mut cwd } = packet.op;
                let mut replies = packet.reply.into_iter().collect::<Vec<_>>();

                while let Ok(queued) = receiver.try_recv() {
                    let ShellSnapshotOp::Refresh { cwd: queued_cwd } = queued.op;
                    cwd = queued_cwd;
                    replies.extend(queued.reply);
                }

                create_and_publish_snapshot(
                    &chaos_home,
                    session_id,
                    cwd.as_path(),
                    &snapshot_shell,
                    &shell_snapshot_tx,
                    session_telemetry.as_ref(),
                )
                .await;

                for reply in replies {
                    let _ = reply.send(());
                }
            }
        });

        Self { mailbox }
    }

    pub(crate) async fn refresh(&self, cwd: PathBuf) -> Result<(), AdapterError> {
        self.mailbox.send(ShellSnapshotOp::Refresh { cwd }).await
    }

    #[cfg(test)]
    async fn refresh_and_wait(&self, cwd: PathBuf) -> Result<(), AdapterError> {
        self.mailbox.call(ShellSnapshotOp::Refresh { cwd }).await
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        tokio::spawn(async move {
            while let Some(packet) = receiver.recv().await {
                if let Some(reply) = packet.reply {
                    let _ = reply.send(());
                }
            }
        });
        Self { mailbox }
    }
}

async fn create_and_publish_snapshot(
    chaos_home: &Path,
    session_id: ProcessId,
    session_cwd: &Path,
    shell: &Shell,
    shell_snapshot_tx: &watch::Sender<Option<Arc<ShellSnapshot>>>,
    session_telemetry: Option<&SessionTelemetry>,
) {
    let timer = session_telemetry
        .map(|telemetry| telemetry.start_timer("chaos.shell_snapshot.duration_ms", &[]));
    let snapshot = ShellSnapshot::try_new(chaos_home, session_id, session_cwd, shell)
        .instrument(info_span!("shell_snapshot", process_id = %session_id))
        .await
        .map(Arc::new);
    let success = snapshot.is_ok();
    let success_tag = if success { "true" } else { "false" };
    if let Some(timer) = timer {
        let _ = timer.map(|timer| timer.record(&[("success", success_tag)]));
    }
    if let Some(session_telemetry) = session_telemetry {
        let mut counter_tags = vec![("success", success_tag)];
        if let Some(failure_reason) = snapshot.as_ref().err() {
            counter_tags.push(("failure_reason", *failure_reason));
        }
        session_telemetry.counter("chaos.shell_snapshot", 1, &counter_tags);
    }

    if let Ok(snapshot) = snapshot {
        drop(shell_snapshot_tx.send_replace(Some(snapshot)));
    }
}

impl ShellSnapshot {
    async fn try_new(
        chaos_home: &Path,
        session_id: ProcessId,
        session_cwd: &Path,
        shell: &Shell,
    ) -> std::result::Result<Self, &'static str> {
        let extension = "sh";
        let generation_id = ProcessId::new();
        let path = chaos_home
            .join(SNAPSHOT_DIR)
            .join(format!("{session_id}.{generation_id}.{extension}"));
        let temp_path = chaos_home
            .join(SNAPSHOT_DIR)
            .join(format!("{session_id}.{generation_id}.tmp"));

        let temp_path =
            match write_shell_snapshot(shell.shell_type.clone(), &temp_path, session_cwd).await {
                Ok(path) => path,
                Err(err) => {
                    tracing::warn!(
                        "Failed to create shell snapshot for {}: {err:?}",
                        shell.name()
                    );
                    return Err("write_failed");
                }
            };
        let mut temp_file = TemporarySnapshotFile::new(temp_path);

        if let Err(err) = validate_snapshot(shell, temp_file.path(), session_cwd).await {
            tracing::error!("Shell snapshot validation failed: {err:?}");
            return Err("validation_failed");
        }

        if let Err(err) = fs::rename(temp_file.path(), &path).await {
            tracing::warn!("Failed to finalize shell snapshot: {err:?}");
            return Err("write_failed");
        }
        temp_file.disarm();
        tracing::info!("Shell snapshot successfully created: {}", path.display());

        Ok(Self {
            path,
            cwd: session_cwd.to_path_buf(),
        })
    }
}

struct TemporarySnapshotFile {
    path: PathBuf,
    armed: bool,
}

impl TemporarySnapshotFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporarySnapshotFile {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            tracing::warn!(
                "Failed to delete temporary shell snapshot at {:?}: {err:?}",
                self.path
            );
        }
    }
}

impl Drop for ShellSnapshot {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path) {
            if err.kind() == ErrorKind::NotFound {
                return;
            }
            tracing::warn!(
                "Failed to delete shell snapshot at {:?}: {err:?}",
                self.path
            );
        }
    }
}

async fn write_shell_snapshot(
    shell_type: ShellType,
    output_path: &Path,
    cwd: &Path,
) -> Result<PathBuf> {
    let shell = get_shell(shell_type.clone(), /*path*/ None)
        .with_context(|| format!("No available shell for {shell_type:?}"))?;

    let raw_snapshot = capture_snapshot(&shell, cwd).await?;
    let snapshot = strip_snapshot_preamble(&raw_snapshot)?;

    let parent = output_path.parent().expect("snapshot path has a parent");
    let parent_display = parent.display();
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("Failed to create snapshot parent {parent_display}"))?;

    let snapshot_path = output_path.display();
    fs::write(output_path, snapshot)
        .await
        .with_context(|| format!("Failed to write snapshot to {snapshot_path}"))?;

    Ok(output_path.to_path_buf())
}

async fn capture_snapshot(shell: &Shell, cwd: &Path) -> Result<String> {
    match shell.shell_type {
        ShellType::Zsh => run_shell_script(shell, &zsh_snapshot_script(), cwd).await,
        ShellType::Bash => run_shell_script(shell, &bash_snapshot_script(), cwd).await,
        ShellType::Sh => run_shell_script(shell, &sh_snapshot_script(), cwd).await,
    }
}

fn strip_snapshot_preamble(snapshot: &str) -> Result<String> {
    let marker = "# Snapshot file";
    let Some(start) = snapshot.find(marker) else {
        bail!("Snapshot output missing marker {marker}");
    };

    Ok(snapshot[start..].to_string())
}

async fn validate_snapshot(shell: &Shell, snapshot_path: &Path, cwd: &Path) -> Result<()> {
    let snapshot_path_display = snapshot_path.display();
    let script = format!("set -e; . \"{snapshot_path_display}\"");
    run_script_with_timeout(
        shell,
        &script,
        SNAPSHOT_TIMEOUT,
        /*use_login_shell*/ false,
        cwd,
    )
    .await
    .map(|_| ())
}

async fn run_shell_script(shell: &Shell, script: &str, cwd: &Path) -> Result<String> {
    run_script_with_timeout(
        shell,
        script,
        SNAPSHOT_TIMEOUT,
        /*use_login_shell*/ true,
        cwd,
    )
    .await
}

async fn run_script_with_timeout(
    shell: &Shell,
    script: &str,
    snapshot_timeout: Duration,
    use_login_shell: bool,
    cwd: &Path,
) -> Result<String> {
    let args = shell.derive_exec_args(script, use_login_shell);
    let shell_name = shell.name();

    // Handler is kept as guard to control the drop. The `mut` pattern is required because .args()
    // returns a ref of handler.
    let mut handler = Command::new(&args[0]);
    handler.args(&args[1..]);
    handler.stdin(Stdio::null());
    handler.current_dir(cwd);
    unsafe {
        handler.pre_exec(|| {
            chaos_pty::process_group::detach_from_tty()?;
            Ok(())
        });
    }
    handler.kill_on_drop(true);
    let output = timeout(snapshot_timeout, handler.output())
        .await
        .map_err(|_| anyhow!("Snapshot command timed out for {shell_name}"))?
        .with_context(|| format!("Failed to execute {shell_name}"))?;

    if !output.status.success() {
        let status = output.status;
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Snapshot command exited with status {status}: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

const ZSH_SNAPSHOT_TEMPLATE: &str = r##"if [[ -n "$ZDOTDIR" ]]; then
  rc="$ZDOTDIR/.zshrc"
else
  rc="$HOME/.zshrc"
fi
[[ -r "$rc" ]] && . "$rc"
print '# Snapshot file'
print '# Unset all aliases to avoid conflicts with functions'
print 'unalias -a 2>/dev/null || true'
print '# Functions'
functions
print ''
setopt_count=$(setopt | wc -l | tr -d ' ')
print "# setopts $setopt_count"
setopt | sed 's/^/setopt /'
print ''
alias_count=$(alias -L | wc -l | tr -d ' ')
print "# aliases $alias_count"
alias -L
print ''
export_lines=$(export -p | awk '
/^(export|declare -x|typeset -x) / {
  line=$0
  name=line
  sub(/^(export|declare -x|typeset -x) /, "", name)
  sub(/=.*/, "", name)
  if (name ~ /^(EXCLUDED_EXPORTS)$/) {
    next
  }
  if (name ~ /^[A-Za-z_][A-Za-z0-9_]*$/) {
    print line
  }
}')
export_count=$(printf '%s\n' "$export_lines" | sed '/^$/d' | wc -l | tr -d ' ')
print "# exports $export_count"
if [[ -n "$export_lines" ]]; then
  print -r -- "$export_lines"
fi
"##;

const BASH_SNAPSHOT_TEMPLATE: &str = r##"if [ -z "$BASH_ENV" ] && [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi
echo '# Snapshot file'
echo '# Unset all aliases to avoid conflicts with functions'
unalias -a 2>/dev/null || true
echo '# Functions'
declare -f
echo ''
bash_opts=$(set -o | awk '$2=="on"{print $1}')
bash_opt_count=$(printf '%s\n' "$bash_opts" | sed '/^$/d' | wc -l | tr -d ' ')
echo "# setopts $bash_opt_count"
if [ -n "$bash_opts" ]; then
  printf 'set -o %s\n' $bash_opts
fi
echo ''
alias_count=$(alias -p | wc -l | tr -d ' ')
echo "# aliases $alias_count"
alias -p
echo ''
export_lines=$(
  while IFS= read -r name; do
    if [[ "$name" =~ ^(EXCLUDED_EXPORTS)$ ]]; then
      continue
    fi
    if [[ ! "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
      continue
    fi
    declare -xp "$name" 2>/dev/null || true
  done < <(compgen -e)
)
export_count=$(printf '%s\n' "$export_lines" | sed '/^$/d' | wc -l | tr -d ' ')
echo "# exports $export_count"
if [ -n "$export_lines" ]; then
  printf '%s\n' "$export_lines"
fi
"##;

const SH_SNAPSHOT_TEMPLATE: &str = r##"if [ -n "$ENV" ] && [ -r "$ENV" ]; then
  . "$ENV"
fi
echo '# Snapshot file'
echo '# Unset all aliases to avoid conflicts with functions'
unalias -a 2>/dev/null || true
echo '# Functions'
if command -v typeset >/dev/null 2>&1; then
  typeset -f
elif command -v declare >/dev/null 2>&1; then
  declare -f
fi
echo ''
if set -o >/dev/null 2>&1; then
  sh_opts=$(set -o | awk '$2=="on"{print $1}')
  sh_opt_count=$(printf '%s\n' "$sh_opts" | sed '/^$/d' | wc -l | tr -d ' ')
  echo "# setopts $sh_opt_count"
  if [ -n "$sh_opts" ]; then
    printf 'set -o %s\n' $sh_opts
  fi
else
  echo '# setopts 0'
fi
echo ''
if alias >/dev/null 2>&1; then
  alias_count=$(alias | wc -l | tr -d ' ')
  echo "# aliases $alias_count"
  alias
  echo ''
else
  echo '# aliases 0'
fi
if export -p >/dev/null 2>&1; then
  export_lines=$(export -p | awk '
/^(export|declare -x|typeset -x) / {
  line=$0
  name=line
  sub(/^(export|declare -x|typeset -x) /, "", name)
  sub(/=.*/, "", name)
  if (name ~ /^(EXCLUDED_EXPORTS)$/) {
    next
  }
  if (name ~ /^[A-Za-z_][A-Za-z0-9_]*$/) {
    print line
  }
}')
  export_count=$(printf '%s\n' "$export_lines" | sed '/^$/d' | wc -l | tr -d ' ')
  echo "# exports $export_count"
  if [ -n "$export_lines" ]; then
    printf '%s\n' "$export_lines"
  fi
else
  export_count=$(env | sort | awk -F= '$1 ~ /^[A-Za-z_][A-Za-z0-9_]*$/ { count++ } END { print count }')
  echo "# exports $export_count"
  env | sort | while IFS='=' read -r key value; do
    case "$key" in
      ""|[0-9]*|*[!A-Za-z0-9_]*|EXCLUDED_EXPORTS) continue ;;
    esac
    escaped=$(printf "%s" "$value" | sed "s/'/'\"'\"'/g")
    printf "export %s='%s'\n" "$key" "$escaped"
  done
fi
"##;

fn build_snapshot_script(template: &str, excluded: &[String]) -> String {
    let excluded_str = excluded.join("|");
    template.replace("EXCLUDED_EXPORTS", &excluded_str)
}

fn excluded_exports() -> Vec<String> {
    EXCLUDED_EXPORT_VARS
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn zsh_snapshot_script() -> String {
    build_snapshot_script(ZSH_SNAPSHOT_TEMPLATE, &excluded_exports())
}

fn bash_snapshot_script() -> String {
    build_snapshot_script(BASH_SNAPSHOT_TEMPLATE, &excluded_exports())
}

fn sh_snapshot_script() -> String {
    build_snapshot_script(SH_SNAPSHOT_TEMPLATE, &excluded_exports())
}

/// Removes shell snapshots that either lack a matching journal process row or
/// whose journal has not been updated within the retention window.
/// The active session id is exempt from cleanup.
pub async fn cleanup_stale_snapshots(
    chaos_home: &Path,
    active_session_id: ProcessId,
) -> Result<()> {
    let snapshot_dir = chaos_home.join(SNAPSHOT_DIR);

    let mut entries = match fs::read_dir(&snapshot_dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    let now = jiff::Timestamp::now().as_second();
    let active_session_id = active_session_id.to_string();
    let journal_store = SqliteJournalStore::open(&chaos_proc::runtime_db_path(chaos_home))
        .await
        .context("open journal database for shell snapshot cleanup")?;

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }

        let path = entry.path();

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let (session_id, _) = match file_name.split_once('.') {
            Some((stem, ext)) => (stem, ext),
            None => {
                remove_snapshot_file(&path).await;
                continue;
            }
        };
        if session_id == active_session_id {
            continue;
        }

        let Ok(process_id) = ProcessId::from_string(session_id) else {
            remove_snapshot_file(&path).await;
            continue;
        };
        let process = match journal_store.get_process(&process_id).await {
            Ok(process) => process,
            Err(err) => {
                tracing::warn!(
                    "Failed to check journal age for snapshot {}: {err:?}",
                    path.display()
                );
                continue;
            }
        };
        let Some(process) = process else {
            remove_snapshot_file(&path).await;
            continue;
        };

        let age_secs = now - process.updated_at.as_second();
        if age_secs >= SNAPSHOT_RETENTION.as_secs() as i64 {
            remove_snapshot_file(&path).await;
        }
    }

    Ok(())
}

async fn remove_snapshot_file(path: &Path) {
    if let Err(err) = fs::remove_file(path).await {
        if err.kind() == ErrorKind::NotFound {
            return;
        }
        tracing::warn!("Failed to delete shell snapshot at {:?}: {err:?}", path);
    }
}

#[cfg(test)]
#[path = "shell_snapshot_tests.rs"]
mod tests;
