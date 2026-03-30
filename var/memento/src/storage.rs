//! Phase 3 — persist and retrieve consolidated memories.

use chaos_proc::Stage1Output;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tracing::warn;
use uuid::Uuid;

const ROLLOUT_SUMMARIES_SUBDIR: &str = "rollout_summaries";
const RAW_MEMORIES_FILENAME: &str = "raw_memories.md";

fn rollout_summaries_dir(root: &Path) -> PathBuf {
    root.join(ROLLOUT_SUMMARIES_SUBDIR)
}

fn raw_memories_file(root: &Path) -> PathBuf {
    root.join(RAW_MEMORIES_FILENAME)
}

async fn ensure_layout(root: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(rollout_summaries_dir(root)).await
}

/// Rebuild `raw_memories.md` from DB-backed stage-1 outputs.
pub async fn rebuild_raw_memories_file_from_memories(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    ensure_layout(root).await?;
    rebuild_raw_memories_file(root, memories, max_raw_memories_for_consolidation).await
}

/// Syncs canonical rollout summary files from DB-backed stage-1 output rows.
pub async fn sync_rollout_summaries_from_memories(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    ensure_layout(root).await?;

    let retained = retained_memories(memories, max_raw_memories_for_consolidation);
    let keep = retained
        .iter()
        .map(rollout_summary_file_stem)
        .collect::<HashSet<_>>();
    prune_rollout_summaries(root, &keep).await?;

    for memory in retained {
        write_rollout_summary_for_thread(root, memory).await?;
    }

    if retained.is_empty() {
        for file_name in ["MEMORY.md", "memory_summary.md"] {
            let path = root.join(file_name);
            if let Err(err) = tokio::fs::remove_file(path).await
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err);
            }
        }

        let skills_dir = root.join("skills");
        if let Err(err) = tokio::fs::remove_dir_all(skills_dir).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(err);
        }
    }

    Ok(())
}

async fn rebuild_raw_memories_file(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    let retained = retained_memories(memories, max_raw_memories_for_consolidation);
    let mut body = String::from("# Raw Memories\n\n");

    if retained.is_empty() {
        body.push_str("No raw memories yet.\n");
        return tokio::fs::write(raw_memories_file(root), body).await;
    }

    body.push_str("Merged stage-1 raw memories (latest first):\n\n");
    for memory in retained {
        writeln!(body, "## Process `{}`", memory.process_id).map_err(raw_memories_format_error)?;
        writeln!(body, "updated_at: {}", memory.source_updated_at)
            .map_err(raw_memories_format_error)?;
        writeln!(body, "cwd: {}", memory.cwd.display()).map_err(raw_memories_format_error)?;
        writeln!(body, "process_ref: {}", memory.process_ref)
            .map_err(raw_memories_format_error)?;
        let rollout_summary_file = format!("{}.md", rollout_summary_file_stem(memory));
        writeln!(body, "rollout_summary_file: {rollout_summary_file}")
            .map_err(raw_memories_format_error)?;
        writeln!(body).map_err(raw_memories_format_error)?;
        body.push_str(memory.raw_memory.trim());
        body.push_str("\n\n");
    }

    tokio::fs::write(raw_memories_file(root), body).await
}

async fn prune_rollout_summaries(root: &Path, keep: &HashSet<String>) -> std::io::Result<()> {
    let dir_path = rollout_summaries_dir(root);
    let mut dir = match tokio::fs::read_dir(&dir_path).await {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".md") else {
            continue;
        };
        if !keep.contains(stem)
            && let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                "failed pruning outdated rollout summary {}: {err}",
                path.display()
            );
        }
    }

    Ok(())
}

async fn write_rollout_summary_for_thread(
    root: &Path,
    memory: &Stage1Output,
) -> std::io::Result<()> {
    let file_stem = rollout_summary_file_stem(memory);
    let path = rollout_summaries_dir(root).join(format!("{file_stem}.md"));

    let mut body = String::new();
    writeln!(body, "process_id: {}", memory.process_id).map_err(rollout_summary_format_error)?;
    writeln!(body, "updated_at: {}", memory.source_updated_at)
        .map_err(rollout_summary_format_error)?;
    writeln!(body, "process_ref: {}", memory.process_ref)
        .map_err(rollout_summary_format_error)?;
    writeln!(body, "cwd: {}", memory.cwd.display()).map_err(rollout_summary_format_error)?;
    if let Some(git_branch) = memory.git_branch.as_deref() {
        writeln!(body, "git_branch: {git_branch}").map_err(rollout_summary_format_error)?;
    }
    writeln!(body).map_err(rollout_summary_format_error)?;
    body.push_str(&memory.rollout_summary);
    body.push('\n');

    tokio::fs::write(path, body).await
}

fn retained_memories(
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> &[Stage1Output] {
    &memories[..memories.len().min(max_raw_memories_for_consolidation)]
}

fn raw_memories_format_error(err: std::fmt::Error) -> std::io::Error {
    std::io::Error::other(format!("format raw memories: {err}"))
}

fn rollout_summary_format_error(err: std::fmt::Error) -> std::io::Error {
    std::io::Error::other(format!("format rollout summary: {err}"))
}

pub fn rollout_summary_file_stem(memory: &Stage1Output) -> String {
    rollout_summary_file_stem_from_parts(
        memory.process_id,
        memory.source_updated_at,
        memory.rollout_slug.as_deref(),
    )
}

pub fn rollout_summary_file_stem_from_parts(
    process_id: chaos_ipc::ProcessId,
    source_updated_at: jiff::Timestamp,
    rollout_slug: Option<&str>,
) -> String {
    const ROLLOUT_SLUG_MAX_LEN: usize = 60;
    const SHORT_HASH_ALPHABET: &[u8; 62] =
        b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const SHORT_HASH_SPACE: u32 = 14_776_336;

    let process_id = process_id.to_string();
    let (timestamp_fragment, short_hash_seed) = match Uuid::parse_str(&process_id) {
        Ok(thread_uuid) => {
            let timestamp = thread_uuid
                .get_timestamp()
                .and_then(|uuid_timestamp| {
                    let (seconds, nanos) = uuid_timestamp.to_unix();
                    i64::try_from(seconds)
                        .ok()
                        .and_then(|secs| jiff::Timestamp::new(secs, nanos as i32).ok())
                })
                .unwrap_or(source_updated_at);
            let short_hash_seed = (thread_uuid.as_u128() & 0xFFFF_FFFF) as u32;
            (
                timestamp.strftime("%Y-%m-%dT%H-%M-%S").to_string(),
                short_hash_seed,
            )
        }
        Err(_) => {
            let mut short_hash_seed = 0u32;
            for byte in process_id.bytes() {
                short_hash_seed = short_hash_seed
                    .wrapping_mul(31)
                    .wrapping_add(u32::from(byte));
            }
            (
                source_updated_at.strftime("%Y-%m-%dT%H-%M-%S").to_string(),
                short_hash_seed,
            )
        }
    };
    let mut short_hash_value = short_hash_seed % SHORT_HASH_SPACE;
    let mut short_hash_chars = ['0'; 4];
    for idx in (0..short_hash_chars.len()).rev() {
        let alphabet_idx = (short_hash_value % SHORT_HASH_ALPHABET.len() as u32) as usize;
        short_hash_chars[idx] = SHORT_HASH_ALPHABET[alphabet_idx] as char;
        short_hash_value /= SHORT_HASH_ALPHABET.len() as u32;
    }
    let short_hash: String = short_hash_chars.iter().collect();
    let file_prefix = format!("{timestamp_fragment}-{short_hash}");

    let Some(raw_slug) = rollout_slug else {
        return file_prefix;
    };

    let mut slug = String::with_capacity(ROLLOUT_SLUG_MAX_LEN);
    for ch in raw_slug.chars() {
        if slug.len() >= ROLLOUT_SLUG_MAX_LEN {
            break;
        }

        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            slug.push('_');
        }
    }

    while slug.ends_with('_') {
        slug.pop();
    }

    if slug.is_empty() {
        file_prefix
    } else {
        format!("{file_prefix}-{slug}")
    }
}
