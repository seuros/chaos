//! Scripted Claude stream-JSON peer, not a model emulator. Native state is keyed
//! by --resume; all tool results come from the real, per-process MCP bridge.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const SESSION: &str = "b033add7-96f0-4967-88ac-20b535ceb2b4";

struct Bridge {
    child: Child,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Bridge {
    fn spawn(config: &Value) -> Result<Self> {
        let server = &config["mcpServers"]["chaos"];
        let mut command = Command::new(server["command"].as_str().ok_or("bridge command")?);
        for arg in server["args"].as_array().ok_or("bridge args")? {
            command.arg(arg.as_str().ok_or("bridge arg")?);
        }
        if let Some(env) = server["env"].as_object() {
            for (key, value) in env {
                command.env(key, value.as_str().ok_or("bridge env value")?);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().ok_or("bridge stdin")?;
        let output = BufReader::new(child.stdout.take().ok_or("bridge stdout")?);
        let mut bridge = Self {
            child,
            input,
            output,
        };
        bridge.request(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2024-11-05","capabilities":{},
                "clientInfo":{"name":"clamp-regression","version":"1"}
            }}),
        )?;
        writeln!(
            bridge.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )?;
        bridge.input.flush()?;
        Ok(bridge)
    }

    fn request(&mut self, request: Value) -> Result<Value> {
        writeln!(self.input, "{request}")?;
        self.input.flush()?;
        loop {
            let mut line = String::new();
            if self.output.read_line(&mut line)? == 0 {
                return Err("bridge EOF before response".into());
            }
            let response: Value = serde_json::from_str(&line)?;
            if response["id"] == request["id"] {
                if response.get("error").is_some() || response["result"]["isError"] == true {
                    return Err(format!("bridge failed: {response}").into());
                }
                return Ok(response["result"].clone());
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}))
    }
}

fn emit(value: Value) -> Result<()> {
    println!("{value}");
    std::io::stdout().flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let option = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    };
    let agy = args.iter().any(|arg| arg == "--disable-slash-commands");
    let resumed = option(if agy { "--conversation" } else { "--resume" });
    if let Some(id) = &resumed {
        assert_eq!(id, SESSION, "wrong native session resumed");
    }
    let root = std::path::PathBuf::from(std::env::var("CLAMP_TEST_ROOT")?);
    let state_path = root.join("work/native.json");
    let mut state: Value = if resumed.is_some() {
        serde_json::from_slice(&std::fs::read(&state_path)?)?
    } else {
        json!({"turn":0})
    };
    let config_path = if agy {
        std::path::PathBuf::from(std::env::var("HOME")?).join(".gemini/config/mcp_config.json")
    } else {
        option("--mcp-config").ok_or("MCP config")?.into()
    };
    let config: Value = serde_json::from_slice(&std::fs::read(config_path)?)?;
    if agy {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("work/invocations.jsonl"))?;
        writeln!(log, "{}", json!({"resumed": resumed}))?;
    }
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        match message[if agy { "event" } else { "type" }].as_str() {
            Some("control_request") => emit(json!({"type":"control_response","response":{
                "subtype":"success","request_id":message["request_id"],"response":{}
            }}))?,
            Some("user") => {
                let content = message["message"]["content"]
                    .as_str()
                    .ok_or("user content")?;
                if agy && std::env::var_os("CLAMP_TEST_RECOVER").is_some() {
                    assert!(resumed.is_none(), "failed native state was reused");
                    assert!(content.contains("<conversation_state>"));
                    emit(json!({"event":"result", "result":{
                        "conversation_id":SESSION,"status":"SUCCESS","response":"RECOVERED_OK"
                    }}))?;
                    return Ok(());
                }
                if agy && std::env::var_os("CLAMP_TEST_FAIL").is_some() {
                    let mut bridge = Bridge::spawn(&config)?;
                    bridge.tool("exec_command", json!({
                        "cmd":"printf 'failed-once\n' >> audit.txt", "workdir":root.join("work"),
                        "yield_time_ms":1000,"shell":"/bin/sh","login":false
                    }))?;
                    emit(json!({"event":"result", "result":{
                        "conversation_id":SESSION,"status":"ERROR","response":"failure after actual tool write"
                    }}))?;
                    return Ok(());
                }
                let turn = state["turn"].as_u64().ok_or("native turn")? + 1;
                assert!(
                    content.contains(&format!("STAGE_{turn}")),
                    "native state not resumed"
                );
                if resumed.is_some() {
                    assert!(
                        !content.contains("<conversation_state>"),
                        "replayed full history"
                    );
                    assert!(!content.contains("STAGE_1"), "replayed completed prompt");
                }
                // A new bridge process must authenticate and route tools on every exec.
                let mut bridge = Bridge::spawn(&config)?;
                let file = if turn == 1 { "first.txt" } else { "second.txt" };
                let read = bridge.tool(
                    "read_file",
                    json!({"file_path":root.join("work").join(file)}),
                )?;
                if turn == 1 {
                    state["first"] = read.clone();
                }
                bridge.tool(
                    "exec_command",
                    json!({
                        "cmd":format!("printf 'turn{turn}\\n' >> audit.txt"),
                        "workdir":root.join("work"),"yield_time_ms":1000,
                        "shell":"/bin/sh","login":false
                    }),
                )?;
                let audit = bridge.tool(
                    "read_file",
                    json!({"file_path":root.join("work/audit.txt")}),
                )?;
                // Only actual tool output is retained. Never open the input files here.
                let text = if turn == 1 {
                    "STAGE_ONE_OK".to_string()
                } else {
                    json!({"first":state["first"],"current":read,"audit":audit}).to_string()
                };
                state["turn"] = json!(turn);
                std::fs::write(&state_path, serde_json::to_vec(&state)?)?;
                if agy {
                    emit(json!({"event":"result", "result":{
                        "conversation_id":SESSION,"status":"SUCCESS","response":text
                    }}))?;
                } else {
                    emit(
                        json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]}}),
                    )?;
                    emit(
                        json!({"type":"result","subtype":"success","result":text,"session_id":SESSION}),
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
