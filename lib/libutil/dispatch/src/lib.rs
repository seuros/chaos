//! Backend dispatch for two-variant enums over the SQLite and Postgres
//! backends.
//!
//! [`backend_dispatch!`] takes a list of `&self` signatures and forwards each
//! to the active variant:
//!
//! ```ignore
//! impl CronStorage for BackendCronStorage {
//!     chaos_dispatch::backend_dispatch! {
//!         async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob>;
//!         async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>>;
//!         fn kind(&self) -> VfsKind;
//!     }
//! }
//! ```
//!
//! The enum must have exactly the variants `Sqlite` and `Postgres`, each
//! holding one value that answers the listed signatures.

/// Forward `&self` methods to the active backend variant.
///
/// See the [module docs](self) for the shape it expects.
#[macro_export]
macro_rules! backend_dispatch {
    () => {};

    (
        $(#[$attr:meta])*
        $vis:vis async fn $name:ident(&self $(, $arg:ident : $argty:ty)* $(,)?) -> $ret:ty;
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        $vis async fn $name(&self $(, $arg: $argty)*) -> $ret {
            match self {
                Self::Sqlite(backend) => backend.$name($($arg),*).await,
                Self::Postgres(backend) => backend.$name($($arg),*).await,
            }
        }
        $crate::backend_dispatch! { $($rest)* }
    };

    (
        $(#[$attr:meta])*
        $vis:vis fn $name:ident(&self $(, $arg:ident : $argty:ty)* $(,)?) -> $ret:ty;
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        $vis fn $name(&self $(, $arg: $argty)*) -> $ret {
            match self {
                Self::Sqlite(backend) => backend.$name($($arg),*),
                Self::Postgres(backend) => backend.$name($($arg),*),
            }
        }
        $crate::backend_dispatch! { $($rest)* }
    };
}
