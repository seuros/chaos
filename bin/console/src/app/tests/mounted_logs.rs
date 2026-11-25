use super::make_test_app;
use crate::side_panel::LogPanelState;
use crate::tracing_subscriber::{self, prelude::*};
use chaos_ipc::ProcessId;
use chaos_kern::user_settings;
use chaos_proc::LogQuery;
use tempfile::tempdir;

#[test]
fn startup_logs_backfill_from_the_mounted_store() {
    crate::tests::run_console_test(|| async {
        check_mounted_logs().await.expect("mounted log fixture");
    });
}

async fn check_mounted_logs() -> anyhow::Result<()> {
    let mut app = make_test_app().await;
    let home = tempdir()?;
    let url = format!("sqlite://{}", home.path().join("mounted.sqlite").display());
    user_settings::set_bootstrap(home.path(), "storage_url", &url)?;
    let mounted = user_settings::open(home.path()).await?;
    app.config.chaos_home = home.path().into();
    app.config.storage_home = home.path().into();
    app.config.storage_url = url;

    let (store, layer) = crate::mounted_log_layer(&app.config).expect("startup logging layer");
    let selected = ProcessId::new();
    let subscriber = tracing_subscriber::registry().with(layer.clone());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("before session selection");
        tracing::info!(process_id = %selected, "selected session");
        tracing::info!(process_id = %ProcessId::new(), "other session");
    });
    layer.flush().await;

    // Check the authoritative mount independently of either console lookup.
    let batch = mounted.tail_backfill(&LogQuery::default(), 10).await?;
    let messages: Vec<_> = batch
        .rows
        .iter()
        .filter_map(|row| row.message.as_deref())
        .collect();
    assert_eq!(
        messages,
        [
            "before session selection",
            "selected session",
            "other session"
        ]
    );

    // Backfill both with the startup handle and with a cold panel that must
    // look up mounted storage for itself. Neither path may open chaos.sqlite.
    for initial_store in [Some(store), None] {
        app.log_state_db = initial_store;
        app.log_panel = LogPanelState::default();
        app.log_panel.set_process_id(Some(selected));
        app.reload_log_panel_backfill().await;
        let lines: Vec<_> = app
            .log_panel
            .render_lines()
            .iter()
            .map(ToString::to_string)
            .collect();
        let contains = |message| lines.iter().any(|line| line.ends_with(message));
        assert!(contains("selected session"), "{lines:?}");
        assert!(contains("before session selection"), "{lines:?}");
        assert!(!contains("other session"), "{lines:?}");
    }
    for file in ["chaos.sqlite", "chaos.sqlite-wal", "chaos.sqlite-shm"] {
        assert!(
            !home.path().join(file).try_exists()?,
            "unexpected fallback {file}"
        );
    }
    Ok(())
}
