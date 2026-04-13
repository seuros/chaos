use super::super::{LogQuery, QueryBuilder, Sqlite};

/// Push a level equality filter onto `builder` when `query.level_upper` is
/// set.  The stored level column is compared case-insensitively by converting
/// it to upper-case in SQL so callers may pass any casing.
pub(super) fn push_level_filter<'a>(builder: &mut QueryBuilder<'a, Sqlite>, query: &'a LogQuery) {
    if let Some(level_upper) = query.level_upper.as_ref() {
        builder
            .push(" AND UPPER(level) = ")
            .push_bind(level_upper.as_str());
    }
}
