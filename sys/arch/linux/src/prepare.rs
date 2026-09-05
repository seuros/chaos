use std::io;

use alcatraz_base::prepared_command::PreparedCommand;
use alcatraz_base::prepared_command::SandboxRequest;

pub fn prepare_command(request: SandboxRequest<'_>) -> io::Result<PreparedCommand> {
    let sandbox_policy = request
        .file_system_policy
        .to_sandbox_policy(request.network_policy, request.sandbox_policy_cwd)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidInput, source))?;
    let sandbox_policy_json = serde_json::to_string(&sandbox_policy).map_err(io::Error::other)?;
    let file_system_policy_json =
        serde_json::to_string(request.file_system_policy).map_err(io::Error::other)?;
    let network_policy_json =
        serde_json::to_string(&request.network_policy).map_err(io::Error::other)?;
    let sandbox_policy_cwd = request
        .sandbox_policy_cwd
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cwd must be valid UTF-8"))?
        .to_string();

    let mut args = vec![
        "--sandbox-policy-cwd".to_string(),
        sandbox_policy_cwd,
        "--sandbox-policy".to_string(),
        sandbox_policy_json,
        "--file-system-sandbox-policy".to_string(),
        file_system_policy_json,
        "--network-sandbox-policy".to_string(),
        network_policy_json,
    ];
    if request.enforce_managed_network {
        args.push("--allow-network-for-proxy".to_string());
    }
    args.push("--".to_string());
    args.extend(request.command);

    Ok(PreparedCommand {
        program: request.executable.to_path_buf(),
        args,
        env: Default::default(),
        arg0: Some("alcatraz".to_string()),
    })
}
