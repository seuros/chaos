use super::*;
use crate::invocation::InvocationAccountingError;
use futures::future::BoxFuture;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Sink {
    reject_begin: AtomicBool,
    reject_receipt: AtomicBool,
    starts: Mutex<Vec<InvocationStart>>,
    receipts: Mutex<Vec<(uuid::Uuid, InvocationReceipt)>>,
    receipt_entered: Notify,
    receipt_release: Notify,
    block_receipt: AtomicBool,
}
impl ProviderInvocationSink for Sink {
    fn begin(
        &self,
        start: InvocationStart,
    ) -> BoxFuture<'_, std::result::Result<bool, InvocationAccountingError>> {
        Box::pin(async move {
            self.starts.lock().unwrap().push(start);
            Ok(!self.reject_begin.load(Ordering::SeqCst))
        })
    }
    fn complete(
        &self,
        id: uuid::Uuid,
        receipt: InvocationReceipt,
    ) -> BoxFuture<'_, std::result::Result<(), InvocationAccountingError>> {
        Box::pin(async move {
            self.receipt_entered.notify_one();
            if self.block_receipt.load(Ordering::SeqCst) {
                self.receipt_release.notified().await;
            }
            if self.reject_receipt.load(Ordering::SeqCst) {
                return Err(InvocationAccountingError);
            }
            self.receipts.lock().unwrap().push((id, receipt));
            Ok(())
        })
    }
}

fn response(usage: Option<chaos_ipc::protocol::TokenUsage>) -> Result<ResponseEvent> {
    Ok(ResponseEvent::Completed {
        response_id: "provider-receipt".into(),
        token_usage: usage,
    })
}

#[tokio::test]
async fn completed_usage_cannot_pass_the_durable_receipt_barrier_or_be_counted_twice() {
    let sink = Arc::new(Sink::default());
    sink.block_receipt.store(true, Ordering::SeqCst);
    let id = uuid::Uuid::new_v4();
    let (tx, rx_event) = tokio::sync::mpsc::channel(4);
    let usage = chaos_ipc::protocol::TokenUsage {
        input_tokens: 19,
        output_tokens: 7,
        total_tokens: 26,
        ..Default::default()
    };
    tx.send(response(Some(usage.clone()))).await.unwrap();
    tx.send(response(Some(usage))).await.unwrap();
    let mut stream = account_response_stream(ResponseStream { rx_event }, sink.clone(), id);
    sink.receipt_entered.notified().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), stream.next())
            .await
            .is_err()
    );
    sink.receipt_release.notify_one();
    assert!(matches!(
        stream.next().await,
        Some(Ok(ResponseEvent::Completed { .. }))
    ));
    assert!(stream.next().await.is_none());
    let receipts = sink.receipts.lock().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].0, id);
    assert_eq!(receipts[0].1.usage.as_ref().unwrap().input_tokens, 19);
}

#[tokio::test]
async fn missing_usage_cancellation_and_failed_receipt_never_manufacture_zero_or_completion() {
    for mode in 0..3 {
        let sink = Arc::new(Sink::default());
        sink.reject_receipt.store(mode == 2, Ordering::SeqCst);
        let (tx, rx_event) = tokio::sync::mpsc::channel(4);
        let mut stream = account_response_stream(
            ResponseStream { rx_event },
            sink.clone(),
            uuid::Uuid::new_v4(),
        );
        if mode == 1 {
            drop(stream);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while sink.receipts.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                sink.receipts.lock().unwrap()[0].1,
                InvocationReceipt::unknown()
            );
        } else {
            tx.send(response(None)).await.unwrap();
            if mode == 2 {
                assert!(matches!(stream.next().await, Some(Err(ChaosErr::Fatal(_)))));
                assert!(sink.receipts.lock().unwrap().is_empty());
            } else {
                assert!(matches!(
                    stream.next().await,
                    Some(Ok(ResponseEvent::Completed {
                        token_usage: None,
                        ..
                    }))
                ));
                assert!(sink.receipts.lock().unwrap()[0].1.usage.is_none());
            }
        }
    }
}

#[tokio::test]
async fn rejected_or_duplicate_begin_cannot_touch_the_provider_client() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = crate::model_provider_info::create_oss_provider_with_base_url(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        crate::model_provider_info::WireApi::Responses,
    );
    let client = ModelClient::new(
        None,
        chaos_ipc::ProcessId::new(),
        "test-provider".into(),
        provider,
        chaos_ipc::protocol::SessionSource::Api,
        chaos_ipc::protocol::ApprovalPolicy::Headless,
        None,
        false,
        None,
        false,
        crate::config::ClampSettings::default(),
    );
    let sink = Arc::new(Sink::default());
    sink.reject_begin.store(true, Ordering::SeqCst);
    client.set_invocation_sink(sink.clone()).unwrap();
    let (_, context) = crate::chaos::make_session_and_context().await;
    let mut session = client.new_session();
    assert!(
        session
            .stream_accounted(
                &uuid::Uuid::new_v4().to_string(),
                InvocationPurpose::Sampling,
                &Prompt::default(),
                &context.model_info,
                &context.session_telemetry,
                None,
                ReasoningSummary::None,
                None,
                None,
            )
            .await
            .is_err()
    );
    assert_eq!(sink.starts.lock().unwrap().len(), 1);
    assert!(sink.receipts.lock().unwrap().is_empty());
    assert!(
        session
            .stream(
                &Prompt::default(),
                &context.model_info,
                &context.session_telemetry,
                None,
                ReasoningSummary::None,
                None,
                None,
            )
            .await
            .is_err(),
        "plain streaming cannot bypass an installed sink"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
