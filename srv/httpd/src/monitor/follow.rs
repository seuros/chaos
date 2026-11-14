//! What feeds the board: this resident's own bus, and the other processes
//! of its home over the fleet bus.

use chaos_kern::activity::fleet::{self, FleetEvent};
use tokio::sync::broadcast;

use super::MonitorState;
use super::board::Change;

impl MonitorState {
    /// Fold every session's activity into the board from now on, so a
    /// monitor that connects later still sees what is running.
    pub(crate) fn follow_activity(&self) {
        let board = self.board.clone();
        let changes = self.changes.clone();
        let mut activity = chaos_kern::activity::subscribe();
        tokio::spawn(async move {
            loop {
                match activity.recv().await {
                    Ok(event) => {
                        let applied = match board.lock() {
                            Ok(mut board) => board.apply(&event),
                            Err(_) => break,
                        };
                        for change in applied {
                            let _ = changes.send(change);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = changes.send(Change::SessionAdded);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Also watch every other Chaos process of this home that forwards its
    /// activity: consoles, `chaos exec`, MCP and ACP servers, taskd.
    pub(crate) async fn follow_fleet(&self, chaos_home: &std::path::Path) {
        let (listening, mut events) = match fleet::listen(chaos_home).await {
            Ok(listening) => listening,
            Err(err) => {
                chaos_snitch::tracing::warn!(%err, "monitor cannot hear other Chaos processes");
                return;
            }
        };
        let board = self.board.clone();
        let changes = self.changes.clone();
        tokio::spawn(async move {
            // Owning the socket here removes it when serve stops.
            let _listening = listening;
            while let Some(event) = events.recv().await {
                let applied = match board.lock() {
                    Ok(mut board) => match &event {
                        FleetEvent::Joined(origin) => board.joined(*origin),
                        FleetEvent::Activity(origin, activity) => {
                            board.apply_remote(*origin, activity)
                        }
                        FleetEvent::Left(origin) => board.left(*origin),
                    },
                    Err(_) => break,
                };
                for change in applied {
                    let _ = changes.send(change);
                }
            }
        });
    }
}
