// Aggregates all former standalone integration tests as modules.
#[path = "no_panic_on_startup.rs"]
mod no_panic_on_startup;
#[path = "status_indicator.rs"]
mod status_indicator;
#[path = "vt100_history.rs"]
mod vt100_history;
#[path = "vt100_live_commit.rs"]
mod vt100_live_commit;
