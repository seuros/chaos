use super::super::{LogQuery, QueryBuilder, Sqlite};
use super::levels::push_level_filter;

/// Append all active filter clauses from `query` to `builder`.
///
/// This includes level, timestamp range, module/file LIKE patterns, process
/// scoping, cursor-based pagination, and full-text search.  Every clause is
/// prefixed with `AND` so callers must already have a `WHERE 1 = 1` anchor.
pub(super) fn push_log_filters<'a>(builder: &mut QueryBuilder<'a, Sqlite>, query: &'a LogQuery) {
    push_level_filter(builder, query);
    if let Some(from_ts) = query.from_ts {
        builder.push(" AND ts >= ").push_bind(from_ts);
    }
    if let Some(to_ts) = query.to_ts {
        builder.push(" AND ts <= ").push_bind(to_ts);
    }
    push_like_filters(builder, "module_path", &query.module_like);
    push_like_filters(builder, "file", &query.file_like);
    if let Some(process_id) = query.related_to_process_id.as_ref() {
        builder.push(" AND (");
        builder.push("process_id = ").push_bind(process_id.as_str());
        if query.include_related_processless {
            builder.push(" OR (process_id IS NULL AND process_uuid IN (");
            builder.push("SELECT process_uuid FROM logs WHERE process_id = ");
            builder.push_bind(process_id.as_str());
            builder.push(
                " AND process_uuid IS NOT NULL ORDER BY ts DESC, ts_nanos DESC, id DESC LIMIT 1",
            );
            builder.push("))");
        }
        builder.push(")");
    } else {
        let has_process_filter = !query.process_ids.is_empty() || query.include_processless;
        if has_process_filter {
            builder.push(" AND (");
            let mut needs_or = false;
            for process_id in &query.process_ids {
                if needs_or {
                    builder.push(" OR ");
                }
                builder.push("process_id = ").push_bind(process_id.as_str());
                needs_or = true;
            }
            if query.include_processless {
                if needs_or {
                    builder.push(" OR ");
                }
                builder.push("process_id IS NULL");
            }
            builder.push(")");
        }
    }
    if let Some(after_id) = query.after_id {
        builder.push(" AND id > ").push_bind(after_id);
    }
    if let Some(search) = query.search.as_ref() {
        builder.push(" AND INSTR(message, ");
        builder.push_bind(search.as_str());
        builder.push(") > 0");
    }
}

/// Append a set of LIKE pattern filters for a single `column`.
///
/// Multiple patterns are OR-ed together so a row matches if its column value
/// contains any of the given substrings.  The group is wrapped in parentheses
/// and prefixed with `AND`.  When `filters` is empty this is a no-op.
pub(super) fn push_like_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    column: &str,
    filters: &'a [String],
) {
    if filters.is_empty() {
        return;
    }
    builder.push(" AND (");
    for (idx, filter) in filters.iter().enumerate() {
        if idx > 0 {
            builder.push(" OR ");
        }
        builder
            .push(column)
            .push(" LIKE '%' || ")
            .push_bind(filter.as_str())
            .push(" || '%'");
    }
    builder.push(")");
}
