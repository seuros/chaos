use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::machine_status::{MachineStatus, ObservationRequest};

pub async fn machine_json(config: &Config, cwd: &Path) -> Result<String, String> {
    #[derive(Serialize)]
    struct Resource {
        scope: &'static str,
        #[serde(flatten)]
        status: MachineStatus,
    }

    let status = ObservationRequest::new(config, cwd).observe().await?;
    super::to_json(
        &Resource {
            scope: "harness_host",
            status,
        },
        "machine",
    )
}
