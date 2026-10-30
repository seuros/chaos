pub(crate) const POLICY_DECISION_EVENT_NAME: &str = super::POLICY_DECISION_EVENT_NAME;

use rama::telemetry::tracing;
use rama::telemetry::tracing::Event;
use rama::telemetry::tracing::Id;
use rama::telemetry::tracing::Metadata;
use rama::telemetry::tracing::Subscriber;
use rama::telemetry::tracing::field::Field;
use rama::telemetry::tracing::field::Visit;
use rama::telemetry::tracing::span::Attributes;
use rama::telemetry::tracing::span::Record;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedEvent {
    pub target: String,
    pub fields: BTreeMap<String, String>,
}

impl CapturedEvent {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Clone, Default)]
struct EventCollector {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    next_span_id: Arc<AtomicU64>,
}

impl EventCollector {
    fn events(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Subscriber for EventCollector {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(CapturedEvent {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn insert(&mut self, field: &Field, value: impl Into<String>) {
        self.fields.insert(field.name().to_string(), value.into());
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.insert(field, value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.insert(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, format!("{value:?}"));
    }
}

pub(crate) async fn capture_events<F, Fut, T>(f: F) -> (T, Vec<CapturedEvent>)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let collector = EventCollector::default();
    let _guard = tracing::dispatcher::set_default(&tracing::Dispatch::new(collector.clone()));
    let output = f().await;
    let events = collector.events();
    (output, events)
}

pub(crate) fn find_event_by_name<'a>(
    events: &'a [CapturedEvent],
    event_name: &str,
) -> Option<&'a CapturedEvent> {
    events
        .iter()
        .find(|event| event.field("event.name") == Some(event_name))
}
