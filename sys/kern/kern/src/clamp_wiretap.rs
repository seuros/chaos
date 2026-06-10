//! Database-backed [`WiretapSink`] for clamp traffic.
//!
//! Bridges `chaos_clamp`'s wiretap proxy to the runtime DB: each captured
//! exchange is converted to a [`ClampExchangeRecord`] and inserted on a spawned
//! task so recording never blocks the proxy's request path.

use chaos_clamp::{WiretapExchange, WiretapSink};
use chaos_proc::{ClampExchangeRecord, RuntimeDbHandle};

/// Records clamp wiretap exchanges into the runtime DB.
pub(crate) struct DbWiretapSink {
    runtime_db: RuntimeDbHandle,
    session_id: Option<String>,
}

impl DbWiretapSink {
    pub(crate) fn new(runtime_db: RuntimeDbHandle, session_id: Option<String>) -> Self {
        Self {
            runtime_db,
            session_id,
        }
    }
}

impl WiretapSink for DbWiretapSink {
    fn record(&self, exchange: WiretapExchange) {
        let record = ClampExchangeRecord {
            session_id: self.session_id.clone(),
            turn_id: None,
            method: exchange.method,
            path: exchange.path,
            status: exchange.status.map(i64::from),
            headers_json: exchange.headers,
            request_json: exchange.request,
            response_body: exchange.response,
            response_truncated: exchange.response_truncated,
        };
        let runtime_db = self.runtime_db.clone();
        tokio::spawn(async move {
            if let Err(err) = runtime_db.record_clamp_exchange(&record).await {
                tracing::warn!("clamp wiretap db insert failed: {err}");
            }
        });
    }
}
