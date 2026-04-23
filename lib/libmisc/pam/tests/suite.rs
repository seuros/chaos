// Aggregates all former standalone integration tests as modules.
#[path = "suite/auth_test_support.rs"]
mod auth_test_support;
#[path = "suite/device_code_login.rs"]
mod device_code_login;
#[path = "suite/login_server_e2e.rs"]
mod login_server_e2e;
