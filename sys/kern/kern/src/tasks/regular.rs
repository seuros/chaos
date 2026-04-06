use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use crate::chaos::TurnContext;
use crate::chaos::run_turn;
use crate::state::TaskKind;
use chaos_ipc::user_input::UserInput;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Default)]
pub(crate) struct RegularTask;

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            let sess = session.clone_session();
            let run_turn_span = trace_span!("run_turn");
            sess.set_server_reasoning_included(/*included*/ false).await;
            run_turn(sess, ctx, input, None, cancellation_token)
                .instrument(run_turn_span)
                .await
        })
    }
}
