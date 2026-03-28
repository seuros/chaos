use sqlx::migrate::Migrator;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx::migrate!("./migrations");
pub(crate) static LOGS_MIGRATOR: Migrator = sqlx::migrate!("./logs_migrations");
pub(crate) static CHAOS_MIGRATOR: Migrator = sqlx::migrate!("./chaos_migrations");
