// Aggregates all former standalone integration tests as modules.
#[cfg(feature = "vt100-tests")]
#[path = "test_backend.rs"]
mod test_backend;

#[path = "suite/model_availability_nux.rs"]
mod model_availability_nux;
#[path = "suite/no_panic_on_startup.rs"]
mod no_panic_on_startup;
#[path = "suite/status_indicator.rs"]
mod status_indicator;
#[path = "suite/vt100_history.rs"]
mod vt100_history;
#[path = "suite/vt100_live_commit.rs"]
mod vt100_live_commit;
