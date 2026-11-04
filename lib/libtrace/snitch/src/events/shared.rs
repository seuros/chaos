use jiff::Timestamp;

macro_rules! log_event {
    ($self:expr, $($fields:tt)*) => {{
        $crate::tracing::event!(
            target: $crate::targets::OTEL_LOG_ONLY_TARGET,
            $crate::tracing::Level::INFO,
            $($fields)*
            event.timestamp = %$crate::events::shared::timestamp(),
            conversation.id = %$self.metadata.conversation_id,
            app.version = %$self.metadata.app_version,
            auth_mode = $self.metadata.auth_mode,
            originator = %$self.metadata.originator,
            terminal.type = %$self.metadata.terminal_type,
            model = %$self.metadata.model,
            slug = %$self.metadata.slug,
        );
    }};
}

macro_rules! trace_event {
    ($self:expr, $($fields:tt)*) => {{
        let current_span = $crate::tracing::Span::current();
        let _telemetry_span = if current_span.is_disabled() {
            // OpenTelemetry exports events as span events, not as standalone
            // trace records. Give otherwise-orphaned safe telemetry events a
            // short root span so they still reach trace-only collectors.
            Some(
                $crate::tracing::info_span!(
                    target: $crate::targets::OTEL_TRACE_SAFE_TARGET,
                    "chaos.telemetry_event",
                )
                .entered(),
            )
        } else {
            None
        };
        $crate::tracing::event!(
            target: $crate::targets::OTEL_TRACE_SAFE_TARGET,
            $crate::tracing::Level::INFO,
            $($fields)*
            event.timestamp = %$crate::events::shared::timestamp(),
            conversation.id = %$self.metadata.conversation_id,
            app.version = %$self.metadata.app_version,
            auth_mode = $self.metadata.auth_mode,
            originator = %$self.metadata.originator,
            terminal.type = %$self.metadata.terminal_type,
            model = %$self.metadata.model,
            slug = %$self.metadata.slug,
        );
    }};
}

macro_rules! log_and_trace_event {
    (
        $self:expr,
        common: { $($common:tt)* },
        log: { $($log:tt)* },
        trace: { $($trace:tt)* },
    ) => {{
        log_event!($self, $($common)* $($log)*);
        trace_event!($self, $($common)* $($trace)*);
    }};
}

pub(crate) use log_and_trace_event;
pub(crate) use log_event;
pub(crate) use trace_event;

pub(crate) fn timestamp() -> String {
    Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}
