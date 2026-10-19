//! Experimental PreToolUse hook: deny by default and audit names only.
//! --allow-chaos-fixture approves only chaos/read_fixture, not real Chaos tools.
//! --allow-kernel-fixture FILE permits only chaos/read_file for that exact file,
//! offset 1 and limit 5. These modes are mutually exclusive.
//! Not a production containment mechanism until runtime coverage is verified.
use std::io::{Read, Write};

#[path = "support/tool_policy.rs"]
mod policy;

enum Outcome {
    Decision(&'static str),
    SimulatedFailure,
}

fn main() -> std::process::ExitCode {
    // Emit a denial even if parsing/auditing fails, except for the explicit
    // failure-injection mode. AGY's handling must be tested separately.
    let result = audit();
    if matches!(result, Ok(Outcome::SimulatedFailure)) {
        // Explicit fault injection only: test whether AGY fails closed when a
        // configured hook exits unsuccessfully without a policy response.
        return std::process::ExitCode::FAILURE;
    }
    let decision = match &result {
        Ok(Outcome::Decision(decision)) => *decision,
        _ => "deny",
    };
    println!(
        "{}",
        serde_json::json!({
            "decision": decision,
            "reason": "Chaos ACP containment experiment: only the opted-in MCP fixture is permitted"
        })
    );
    if result.is_err() {
        eprintln!("Hook input or audit unavailable; denial remains requested");
    }
    std::process::ExitCode::SUCCESS
}

fn audit() -> Result<Outcome, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let audit_path = args.next().ok_or("missing private audit path")?;
    let mut simulate_failure = false;
    let mut allow_fixture = false;
    let mut kernel_fixture = None;
    while let Some(flag) = args.next() {
        if flag == "--simulate-failure" && !simulate_failure {
            simulate_failure = true;
        } else if flag == "--allow-chaos-fixture" && !allow_fixture {
            allow_fixture = true;
        } else if flag == "--allow-kernel-fixture" && kernel_fixture.is_none() {
            let file = args.next().ok_or("missing kernel fixture path")?;
            let file = file.into_string().map_err(|_| "non-UTF8 fixture path")?;
            if !std::path::Path::new(&file).is_absolute() {
                return Err("kernel fixture path must be absolute".into());
            }
            kernel_fixture = Some(file);
        } else {
            return Err("unknown or repeated hook argument".into());
        }
    }
    if allow_fixture && kernel_fixture.is_some() {
        return Err("fixture modes are mutually exclusive".into());
    }
    // Record process entry before parsing stdin, distinguishing "hook started
    // but failed" from no observed invocation.
    let mut file = std::fs::OpenOptions::new().append(true).open(audit_path)?;
    writeln!(file, "{}", serde_json::json!({"event": "invoked"}))?;
    if simulate_failure {
        return Ok(Outcome::SimulatedFailure);
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    if input.len() > 1024 * 1024 {
        return Err("hook input too large".into());
    }
    let request: serde_json::Value = serde_json::from_slice(&input)?;
    let name = request
        .pointer("/toolCall/name")
        .and_then(serde_json::Value::as_str)
        .and_then(policy::safe_identifier)
        .unwrap_or("unknown");
    let decision = kernel_fixture.as_deref().map_or_else(
        || policy::decision(&request, allow_fixture),
        |file| policy::kernel_fixture_decision(&request, file),
    );
    let argument_keys: Vec<_> = request
        .pointer("/toolCall/args")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|object| object.keys())
        .filter_map(|key| policy::safe_identifier(key))
        .take(16)
        .collect();
    let argument_types: Vec<_> = request
        .pointer("/toolCall/args")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|object| object.iter())
        .filter_map(|(key, value)| {
            policy::safe_identifier(key).map(|key| {
                let kind = match value {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object",
                };
                (key, kind)
            })
        })
        .take(16)
        .collect();
    // The test creates this private file; never create/truncate an arbitrary
    // path or retain arguments, credentials, results or conversation text.
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "event": if decision == "deny" { "denied" } else { "allowed" },
            "name": name,
            "argument_keys": argument_keys,
            "argument_types": argument_types,
            "decision": decision
        })
    )?;
    Ok(Outcome::Decision(decision))
}
