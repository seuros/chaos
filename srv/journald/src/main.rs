use std::path::PathBuf;

use chaos_ipc::product::OS_NAME;
use clap::CommandFactory;
use clap::FromArgMatches;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use chaos_journald::JournalServerConfig;
use chaos_journald::default_socket_path;
use chaos_journald::run_sqlite_journal_server;
use chaos_journald::sqlite_db_path;

#[derive(Debug, Parser)]
#[command(name = "chaos_journald")]
struct Cli {
    /// Unix domain socket path to bind.
    #[arg(long = "socket", value_name = "PATH")]
    socket_path: Option<PathBuf>,

    /// SQLite journal database path.
    #[arg(long = "db", value_name = "PATH")]
    sqlite_db_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::from_arg_matches(
        &Cli::command()
            .about(format!("{OS_NAME} local session journal daemon"))
            .get_matches(),
    )?;
    let config = JournalServerConfig {
        socket_path: match cli.socket_path {
            Some(path) => path,
            None => default_socket_path()?,
        },
        sqlite_db_path: match cli.sqlite_db_path {
            Some(path) => path,
            None => sqlite_db_path()?,
        },
    };

    run_sqlite_journal_server(config).await
}

fn init_tracing() {
    let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    let filter = EnvFilter::from_default_env();
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(filter)
        .try_init();
}
