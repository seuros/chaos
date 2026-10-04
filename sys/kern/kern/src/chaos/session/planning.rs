use crate::chaos::INITIAL_SUBMIT_ID;
use chaos_ipc::protocol::{Event, EventMsg};

use super::Session;

impl Session {
    /// Send the attached plan as a client-only update.
    pub(crate) async fn send_attached_plan(&self) -> anyhow::Result<()> {
        if let Some(db) = self.runtime_db()
            && let Some(plan) = db
                .planning_attachment(&self.conversation_id.to_string())
                .await?
        {
            let snapshot = db.planning_read(&plan, 0).await?;
            self.send_event_raw(Event {
                id: INITIAL_SUBMIT_ID.into(),
                msg: EventMsg::PlanUpdate(snapshot.into()),
            })
            .await;
        }
        Ok(())
    }
}
