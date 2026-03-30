use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_proc::ProcessMetadataBuilder;
use jiff::Timestamp;

pub(crate) fn builder_from_session_meta(
    session_meta: &SessionMetaLine,
) -> Option<ProcessMetadataBuilder> {
    let created_at = parse_timestamp_to_utc(session_meta.meta.timestamp.as_str())?;
    let mut builder = ProcessMetadataBuilder::new(
        session_meta.meta.id,
        created_at,
        session_meta.meta.source.clone(),
    );
    builder.model_provider = session_meta.meta.model_provider.clone();
    builder.agent_nickname = session_meta.meta.agent_nickname.clone();
    builder.agent_role = session_meta.meta.agent_role.clone();
    builder.cwd = session_meta.meta.cwd.clone();
    builder.cli_version = Some(session_meta.meta.cli_version.clone());
    builder.sandbox_policy = SandboxPolicy::new_read_only_policy();
    builder.approval_mode = AskForApproval::OnRequest;
    if let Some(git) = session_meta.git.as_ref() {
        builder.git_sha = git.commit_hash.clone();
        builder.git_branch = git.branch.clone();
        builder.git_origin_url = git.repository_url.clone();
    }
    Some(builder)
}

pub(crate) fn builder_from_items(items: &[RolloutItem]) -> Option<ProcessMetadataBuilder> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => builder_from_session_meta(meta_line),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

fn parse_timestamp_to_utc(value: &str) -> Option<Timestamp> {
    value
        .parse::<Timestamp>()
        .ok()
        .map(|ts| Timestamp::from_second(ts.as_second()).unwrap_or(ts))
}
