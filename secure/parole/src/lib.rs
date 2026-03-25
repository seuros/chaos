//! Chaos Parole — permission escalation and policy enforcement.
//!
//! Parole manages the "can this run?" question: exec policy evaluation,
//! sandbox permission grants, additional permission merging, and
//! escalation decisions. One crate to unify the scattered approval
//! logic currently spread across core's sandboxing, exec_policy, and
//! config modules.
