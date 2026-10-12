extern crate usage as usage_rs;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chaos_proc::LogQuery;
use chaos_proc::LogRow;
use chaos_proc::LogTailCursor;
use chaos_proc::RuntimeDbHandle;
use owo_colors::OwoColorize;

const STORAGE_URL_ENV: &str = "CHAOS_STORAGE_URL";

#[derive(Debug, usage::Cli)]
#[usage(
    bin = "chaos-state-logs",
    about = "Tail FreeChaOS logs from the runtime database with simple filters",
    unknown_flags = "error",
    args_override_self = false
)]
struct Args {
    /// Runtime database URL (`sqlite:///path/chaos.sqlite` or
    /// `postgres://...`). Defaults to `CHAOS_STORAGE_URL`. The database must
    /// already exist; this tool never creates one.
    #[usage(long, value_name = "URL")]
    storage_url: Option<String>,

    /// Directory keying local state for the handle. Defaults to the current
    /// directory; it does not select the database.
    #[usage(long)]
    chaos_home: Option<PathBuf>,

    /// Log level to match exactly (case-insensitive).
    #[usage(long)]
    level: Option<String>,

    /// Start timestamp (RFC3339 or unix seconds).
    #[usage(long, value_name = "RFC3339|UNIX")]
    from: Option<String>,

    /// End timestamp (RFC3339 or unix seconds).
    #[usage(long, value_name = "RFC3339|UNIX")]
    to: Option<String>,

    /// Substring match on module_path. Repeat to include multiple substrings.
    #[usage(long = "module")]
    module: Vec<String>,

    /// Substring match on file path. Repeat to include multiple substrings.
    #[usage(long = "file")]
    file: Vec<String>,

    /// Match one or more thread ids. Repeat to include multiple processes.
    #[usage(long = "thread-id")]
    process_id: Vec<String>,

    /// Substring match against the log message.
    #[usage(long)]
    search: Option<String>,

    /// Include logs that do not have a thread id.
    #[usage(long)]
    processless: bool,

    /// Number of matching rows to show before tailing.
    #[usage(long, default = "200")]
    backfill: usize,

    /// Poll interval in milliseconds.
    #[usage(long, default = "500")]
    poll_ms: u64,

    /// Show compact output with only time, level, and message.
    #[usage(long)]
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
    let filter = build_filter(&args)?;
    let runtime = open_runtime(&args).await?;

    let mut cursor = print_backfill(&runtime, &filter, args.backfill, args.compact).await?;

    let poll_interval = Duration::from_millis(args.poll_ms);
    loop {
        let polled = fetch_new_rows(&runtime, &filter, &cursor).await?;
        for row in &polled.rows {
            println!("{}", format_row(row, args.compact));
        }
        cursor = polled.cursor;
        tokio::time::sleep(poll_interval).await;
    }
}

/// Open the mounted backend named by `--storage-url` or `CHAOS_STORAGE_URL`.
async fn open_runtime(args: &Args) -> anyhow::Result<RuntimeDbHandle> {
    let storage_url = args
        .storage_url
        .clone()
        .or_else(|| std::env::var(STORAGE_URL_ENV).ok())
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .with_context(|| {
            format!("no runtime database configured; pass --storage-url or set {STORAGE_URL_ENV}")
        })?;
    let chaos_home = match args.chaos_home.clone() {
        Some(home) => home,
        None => std::env::current_dir()?,
    };
    let provider = "logs-client".to_string();
    if storage_url.starts_with("postgres://") || storage_url.starts_with("postgresql://") {
        let pool = chaos_proc::open_runtime_db_postgres_url(&storage_url)
            .await
            .context("failed to open the PostgreSQL runtime database")?;
        return Ok(RuntimeDbHandle::from_postgres_pool(
            chaos_home, provider, pool,
        ));
    }
    if let Some(path) = sqlite_file_path(&storage_url) {
        anyhow::ensure!(
            path.is_file(),
            "SQLite runtime database {} does not exist; this tool never creates one",
            path.display()
        );
    }
    let pool = chaos_proc::open_runtime_db_url(&storage_url)
        .await
        .context("failed to open the SQLite runtime database")?;
    Ok(RuntimeDbHandle::from_sqlite_pool(
        chaos_home, provider, pool,
    ))
}

/// The file behind a `sqlite:` URL, or `None` for in-memory or non-SQLite URLs.
fn sqlite_file_path(storage_url: &str) -> Option<PathBuf> {
    let rest = storage_url
        .strip_prefix("sqlite://")
        .or_else(|| storage_url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or_default();
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(PathBuf::from(path))
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
    runtime: &RuntimeDbHandle,
    filter: &LogFilter,
    backfill: usize,
    compact: bool,
) -> anyhow::Result<LogTailCursor> {
    let backfill_batch = fetch_backfill(runtime, filter, backfill).await?;
    for row in backfill_batch.rows {
        println!("{}", format_row(&row, compact));
    }
    Ok(backfill_batch.cursor)
}

async fn fetch_backfill(
    runtime: &RuntimeDbHandle,
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
    runtime: &RuntimeDbHandle,
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
