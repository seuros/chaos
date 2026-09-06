//! Foreground recovery supervisor. No model runner, task database, or policy
//! engine lives here; all three remain kernel services.

use chaos_argv::Arg0DispatchPaths;
use chaos_coreboot::CoreBoot;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::protocol::{EventMsg, SessionSource};
use chaos_kern::background_recovery;
use chaos_kern::config::{ConfigBuilder, ConfigOverrides};
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_session::background::{BackgroundWait, DEFAULT_BACKGROUND_TIMEOUT, WaitEvent};
use std::time::Duration;

#[derive(Debug, Default, usage::Args)]
pub struct TaskdCli {
    /// Scan for eligible durable sessions once, then exit.
    #[usage(long = "once")]
    pub once: bool,
}

pub async fn run_main(
    paths: Arg0DispatchPaths,
    overrides: CliConfigOverrides,
    cli: TaskdCli,
) -> anyhow::Result<()> {
    let overrides = overrides.parse_overrides().map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(overrides.clone())
        .harness_overrides(ConfigOverrides {
            alcatraz_exe: Some(paths.alcatraz_exe.clone()),
            ..Default::default()
        })
        .build()
        .await?;
    chaos_kern::runtime_db::mount_vfs_for_startup(&config).await?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let mut cursor = None;
    loop {
        let (candidates, next) = tokio::select! {
            _ = &mut shutdown => break,
            result = background_recovery::discover(&config, cursor.as_ref()) => result?,
        };
        // One recovered session at a time is intentional: bounded model spend
        // and exclusive writer leases, independent of how many taskd processes
        // the operator starts.
        for candidate in candidates {
            let process_id = candidate.process_id;
            let config = match candidate
                .load_config(paths.alcatraz_exe.clone(), overrides.clone())
                .await
            {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(%process_id, %error, "recovery blocked");
                    continue;
                }
            };
            let core = CoreBoot::boot(
                &config,
                SessionSource::Api,
                CollaborationModesConfig {
                    default_mode_request_user_input: false,
                },
            );
            // Resume claims and continuously renews the canonical writer
            // lease before the kernel can admit any continuation.
            let recovered = match core
                .process_table
                .resume_process(config, process_id, core.auth_manager.clone(), None)
                .await
            {
                Ok(recovered) => recovered,
                Err(error) => {
                    tracing::debug!(%process_id, %error, "recovery deferred (possibly live owner)");
                    continue;
                }
            };
            let process = recovered.process;
            let mut wait = BackgroundWait::for_recovery(&process, DEFAULT_BACKGROUND_TIMEOUT);
            let stop = loop {
                tokio::select! {
                    _ = &mut shutdown => break true,
                    event = wait.next(&process) => match event {
                        WaitEvent::Complete => break false,
                        WaitEvent::Stopped(reason) => {
                            tracing::warn!(%process_id, %reason, "recovery paused");
                            break false;
                        }
                        WaitEvent::Event(event) => match event.msg {
                            EventMsg::Error(error) => {
                                tracing::warn!(%process_id, message = %error.message, "recovery turn failed");
                                break false;
                            }
                            EventMsg::ShutdownComplete | EventMsg::TurnAborted(_) => break false,
                            _ => {}
                        }
                    }
                }
            };
            match tokio::time::timeout(
                Duration::from_secs(10),
                process.release_background_ownership(),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%process_id, %error, "recovery shutdown failed"),
                Err(error) => {
                    tracing::warn!(%process_id, %error, "recovery shutdown exceeded grace period")
                }
            }
            core.process_table.remove_process(&process_id).await;
            if stop {
                return Ok(());
            }
        }
        cursor = next;
        if cursor.is_none() {
            if cli.once {
                break;
            }
            tokio::select! {
                _ = &mut shutdown => break,
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
