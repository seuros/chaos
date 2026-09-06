//! Session-owned work registry. Producers report lifecycle changes; only the
//! session runner admits model turns. No transport or UI policy belongs here.

mod mcp;
mod recovery;
mod registry;
pub(crate) use registry::TaskRegistry;
