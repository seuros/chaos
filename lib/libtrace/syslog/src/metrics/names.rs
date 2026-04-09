pub const TOOL_CALL_COUNT_METRIC: &str = "chaos.tool.call";
pub const TOOL_CALL_DURATION_METRIC: &str = "chaos.tool.call.duration_ms";
pub const API_CALL_COUNT_METRIC: &str = "chaos.api_request";
pub const API_CALL_DURATION_METRIC: &str = "chaos.api_request.duration_ms";
pub const SSE_EVENT_COUNT_METRIC: &str = "chaos.sse_event";
pub const SSE_EVENT_DURATION_METRIC: &str = "chaos.sse_event.duration_ms";
pub const RESPONSES_API_OVERHEAD_DURATION_METRIC: &str = "chaos.responses_api_overhead.duration_ms";
pub const RESPONSES_API_INFERENCE_TIME_DURATION_METRIC: &str =
    "chaos.responses_api_inference_time.duration_ms";
pub const RESPONSES_API_ENGINE_IAPI_TTFT_DURATION_METRIC: &str =
    "chaos.responses_api_engine_iapi_ttft.duration_ms";
pub const RESPONSES_API_ENGINE_SERVICE_TTFT_DURATION_METRIC: &str =
    "chaos.responses_api_engine_service_ttft.duration_ms";
pub const RESPONSES_API_ENGINE_IAPI_TBT_DURATION_METRIC: &str =
    "chaos.responses_api_engine_iapi_tbt.duration_ms";
pub const RESPONSES_API_ENGINE_SERVICE_TBT_DURATION_METRIC: &str =
    "chaos.responses_api_engine_service_tbt.duration_ms";
pub const TURN_E2E_DURATION_METRIC: &str = "chaos.turn.e2e_duration_ms";
pub const TURN_TTFT_DURATION_METRIC: &str = "chaos.turn.ttft.duration_ms";
pub const TURN_TTFM_DURATION_METRIC: &str = "chaos.turn.ttfm.duration_ms";
pub const TURN_NETWORK_PROXY_METRIC: &str = "chaos.turn.network_proxy";
pub const TURN_TOOL_CALL_METRIC: &str = "chaos.turn.tool.call";
pub const TURN_TOKEN_USAGE_METRIC: &str = "chaos.turn.token_usage";
pub const THREAD_STARTED_METRIC: &str = "chaos.thread.started";
