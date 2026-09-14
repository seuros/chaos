use std::future::Future;
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::machine_status::{MachineStatus, ObservationRequest};

pub async fn machine_json(config: &Config, cwd: &Path) -> Result<String, String> {
    machine_json_with_observer(
        config,
        cwd,
        |request| async move { request.observe().await },
    )
    .await
}

async fn machine_json_with_observer<F: Future<Output = Result<MachineStatus, String>>>(
    config: &Config,
    cwd: &Path,
    observe: impl FnOnce(ObservationRequest) -> F,
) -> Result<String, String> {
    #[derive(Serialize)]
    struct Resource {
        scope: &'static str,
        #[serde(flatten)]
        status: MachineStatus,
    }

    let status = observe(ObservationRequest::new(config, cwd)).await?;
    super::to_json(
        &Resource {
            scope: "harness_host",
            status,
        },
        "machine",
    )
}

#[cfg(test)]
mod tests;
