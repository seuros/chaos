use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub(super) struct Peer {
    child: Child,
    writer: ChildStdin,
    reader: Lines<BufReader<ChildStdout>>,
}

impl Peer {
    pub async fn open(command: &mut Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let writer = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut peer = Self {
            child,
            writer,
            reader,
        };
        let (initialized, _) = peer
            .request(1, "initialize", json!({"protocolVersion":1}))
            .await;
        assert_eq!(initialized["result"]["agentInfo"]["name"], "chaos");
        assert_eq!(
            initialized["result"]["agentInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            initialized["result"]["agentCapabilities"]["loadSession"],
            true
        );
        peer
    }

    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> (Value, Vec<Value>) {
        let frame = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        self.writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        let mut updates = Vec::new();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(20), self.reader.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let frame: Value =
                serde_json::from_str(&line).expect("stdout must contain only ACP JSON frames");
            if frame["id"] == id {
                return (frame, updates);
            }
            assert_eq!(frame["method"], "session/update", "{frame}");
            updates.push(frame);
        }
    }

    pub async fn prompt(&mut self, id: u64, session: &Value, expected: &str) {
        let (done, updates) = self
            .request(
                id,
                "session/prompt",
                json!({
                    "sessionId":session,"prompt":[{"type":"text","text":"fixture prompt"}]
                }),
            )
            .await;
        assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
        let mut text = String::new();
        for frame in updates {
            assert_eq!(frame["params"]["sessionId"], *session);
            if frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                text.push_str(
                    frame["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap(),
                );
            }
        }
        assert_eq!(text, expected);
    }

    pub async fn close(mut self) {
        drop(self.writer);
        assert!(
            tokio::time::timeout(Duration::from_secs(6), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(self.reader.next_line().await.unwrap().is_none());
    }
}
