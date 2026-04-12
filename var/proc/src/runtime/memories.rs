use super::processes::push_process_filters;
use super::processes::push_process_order_and_limit;
use jiff::ToSpan;

const JOB_KIND_MEMORY_STAGE1: &str = "memory_stage1";
const JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL: &str = "memory_consolidate_global";
const MEMORY_CONSOLIDATION_JOB_KEY: &str = "global";

const DEFAULT_RETRY_REMAINING: i64 = 3;

fn whole_days_as_hours(days: i64) -> jiff::Span {
    days.saturating_mul(24).hours()
}

mod consolidation;
mod lifecycle;
mod outputs;
mod stage1;

use consolidation::enqueue_global_consolidation_with_executor;

#[cfg(test)]
mod tests;
