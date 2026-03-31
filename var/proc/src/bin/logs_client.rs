use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chaos_proc::LogQuery;
use chaos_proc::LogRow;
use chaos_proc::LogTailCursor;
use chaos_proc::StateRuntime;
use clap::Parser;
use owo_colors::OwoColorize;

#[derive(Debug, Parser)]
#[command(name = "codex-state-logs")]
#[command(about = "Tail Chaos logs from the dedicated logs SQLite DB with simple filters")]
struct Args {
    /// Path to the ChaOS home directory. Defaults to `CHAOS_HOME`, then
    /// `~/.chaos`.
    #[arg(long)]
    chaos_home: Option<PathBuf>,

    /// Direct path to the logs SQLite database. Overrides --chaos-home.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Log level to match exactly (case-insensitive).
    #[arg(long)]
    level: Option<String>,

    /// Start timestamp (RFC3339 or unix seconds).
    #[arg(long, value_name = "RFC3339|UNIX")]
    from: Option<String>,

    /// End timestamp (RFC3339 or unix seconds).
    #[arg(long, value_name = "RFC3339|UNIX")]
    to: Option<String>,

    /// Substring match on module_path. Repeat to include multiple substrings.
    #[arg(long = "module")]
    module: Vec<String>,

    /// Substring match on file path. Repeat to include multiple substrings.
    #[arg(long = "file")]
    file: Vec<String>,

    /// Match one or more thread ids. Repeat to include multiple processes.
    #[arg(long = "thread-id")]
    process_id: Vec<String>,

    /// Substring match against the log message.
    #[arg(long)]
    search: Option<String>,

    /// Include logs that do not have a thread id.
    #[arg(long)]
    processless: bool,

    /// Number of matching rows to show before tailing.
    #[arg(long, default_value_t = 200)]
    backfill: usize,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    poll_ms: u64,

    /// Show compact output with only time, level, and message.
    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Clone)]
struct LogFilter {
    level_upper: Option<String>,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    module_like: Vec<String>,
    file_like: Vec<String>,
    process_ids: Vec<String>,
    search: Option<String>,
    include_processless: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let db_path = resolve_db_path(&args)?;
    let filter = build_filter(&args)?;
    let chaos_home = db_path
        .parent()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| PathBuf::from("."));
    let runtime = StateRuntime::init(chaos_home, "logs-client".to_string()).await?;

    let mut cursor = print_backfill(runtime.as_ref(), &filter, args.backfill, args.compact).await?;

    let poll_interval = Duration::from_millis(args.poll_ms);
    loop {
        let polled = fetch_new_rows(runtime.as_ref(), &filter, &cursor).await?;
        for row in &polled.rows {
            println!("{}", format_row(row, args.compact));
        }
        cursor = polled.cursor;
        tokio::time::sleep(poll_interval).await;
    }
}

fn resolve_db_path(args: &Args) -> anyhow::Result<PathBuf> {
    if let Some(db) = args.db.as_ref() {
        return Ok(db.clone());
    }

    let chaos_home = args
        .chaos_home
        .clone()
        .or_else(resolve_home_from_env)
        .unwrap_or_else(default_codex_home);
    Ok(chaos_proc::logs_db_path(chaos_home.as_path()))
}

fn resolve_home_from_env() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("CHAOS_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

fn default_codex_home() -> PathBuf {
    PathBuf::from(".chaos")
}

fn build_filter(args: &Args) -> anyhow::Result<LogFilter> {
    let from_ts = args
        .from
        .as_deref()
        .map(parse_timestamp)
        .transpose()
        .context("failed to parse --from")?;
    let to_ts = args
        .to
        .as_deref()
        .map(parse_timestamp)
        .transpose()
        .context("failed to parse --to")?;

    let level_upper = args.level.as_ref().map(|level| level.to_ascii_uppercase());
    let module_like = args
        .module
        .iter()
        .filter(|module| !module.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let file_like = args
        .file
        .iter()
        .filter(|file| !file.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let process_ids = args
        .process_id
        .iter()
        .filter(|process_id| !process_id.is_empty())
        .cloned()
        .collect::<Vec<_>>();

    Ok(LogFilter {
        level_upper,
        from_ts,
        to_ts,
        module_like,
        file_like,
        process_ids,
        search: args.search.clone(),
        include_processless: args.processless,
    })
}

fn parse_timestamp(value: &str) -> anyhow::Result<i64> {
    if let Ok(secs) = value.parse::<i64>() {
        return Ok(secs);
    }

    let ts: jiff::Timestamp = value
        .parse()
        .with_context(|| format!("expected RFC3339 or unix seconds, got {value}"))?;
    Ok(ts.as_second())
}

async fn print_backfill(
    runtime: &StateRuntime,
    filter: &LogFilter,
    backfill: usize,
    compact: bool,
) -> anyhow::Result<LogTailCursor> {
    let backfill_batch = fetch_backfill(runtime, filter, backfill).await?;
    for row in backfill_batch.rows {
        println!("{}", format_row(row, compact));
    }
    Ok(backfill_batch.cursor)
}

async fn fetch_backfill(
    runtime: &StateRuntime,
    filter: &LogFilter,
    backfill: usize,
) -> anyhow::Result<chaos_proc::LogTailBatch> {
    let query = to_log_query(filter);
    runtime
        .tail_backfill(&query, backfill)
        .await
        .context("failed to fetch backfill logs")
}

async fn fetch_new_rows(
    runtime: &StateRuntime,
    filter: &LogFilter,
    cursor: &LogTailCursor,
) -> anyhow::Result<chaos_proc::LogTailBatch> {
    let query = to_log_query(filter);
    runtime
        .tail_poll(&query, cursor, None)
        .await
        .context("failed to fetch new logs")
}

fn to_log_query(filter: &LogFilter) -> LogQuery {
    LogQuery {
        level_upper: filter.level_upper.clone(),
        from_ts: filter.from_ts,
        to_ts: filter.to_ts,
        module_like: filter.module_like.clone(),
        file_like: filter.file_like.clone(),
        process_ids: filter.process_ids.clone(),
        search: filter.search.clone(),
        include_processless: filter.include_processless,
        related_to_process_id: None,
        include_related_processless: false,
        after_id: None,
        limit: None,
        descending: false,
    }
}

fn format_row(row: &LogRow, compact: bool) -> String {
    let timestamp = formatter::ts(row.ts, row.ts_nanos, compact);
    let level = row.level.as_str();
    let target = row.target.as_str();
    let message = row.message.as_deref().unwrap_or("");
    let level_colored = formatter::level(level);
    let timestamp_colored = timestamp.dimmed().to_string();
    let process_id = row.process_id.as_deref().unwrap_or("-");
    let process_id_colored = process_id.blue().dimmed().to_string();
    let target_colored = target.dimmed().to_string();
    let message_colored = heuristic_formatting(message);
    if compact {
        format!("{timestamp_colored} {level_colored} {message_colored}")
    } else {
        format!(
            "{timestamp_colored} {level_colored} [{process_id_colored}] {target_colored} - {message_colored}"
        )
    }
}

fn heuristic_formatting(message: &str) -> String {
    if matcher::apply_patch(message) {
        formatter::apply_patch(message)
    } else {
        message.bold().to_string()
    }
}

mod matcher {
    pub(super) fn apply_patch(message: &str) -> bool {
        message.starts_with("ToolCall: apply_patch")
    }
}

mod formatter {
    use owo_colors::OwoColorize;

    pub(super) fn apply_patch(message: &str) -> String {
        message
            .lines()
            .map(|line| {
                if line.starts_with('+') {
                    line.green().bold().to_string()
                } else if line.starts_with('-') {
                    line.red().bold().to_string()
                } else {
                    line.bold().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn ts(ts: i64, ts_nanos: i64, compact: bool) -> String {
        let nanos = i32::try_from(ts_nanos).unwrap_or(0);
        match jiff::Timestamp::new(ts, nanos) {
            Ok(dt) if compact => dt.strftime("%H:%M:%S").to_string(),
            Ok(dt) => dt.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            Err(_) => format!("{ts}.{ts_nanos:09}Z"),
        }
    }

    pub(super) fn level(level: &str) -> String {
        let padded = format!("{level:<5}");
        if level.eq_ignore_ascii_case("error") {
            return padded.red().bold().to_string();
        }
        if level.eq_ignore_ascii_case("warn") {
            return padded.yellow().bold().to_string();
        }
        if level.eq_ignore_ascii_case("info") {
            return padded.green().bold().to_string();
        }
        if level.eq_ignore_ascii_case("debug") {
            return padded.blue().bold().to_string();
        }
        if level.eq_ignore_ascii_case("trace") {
            return padded.magenta().bold().to_string();
        }
        padded.bold().to_string()
    }
}
