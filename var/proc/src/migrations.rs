use sqlx::migrate::Migrator;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/sqlite");
pub(crate) static POSTGRES_STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/postgres");

#[cfg(test)]
mod turn_model_provider_tests;

#[cfg(test)]
mod tests;
