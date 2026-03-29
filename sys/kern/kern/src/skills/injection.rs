use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use crate::analytics_client::AnalyticsEventsClient;
use crate::analytics_client::TrackEventsContext;
use crate::instructions::SkillInstructions;
use crate::mention_syntax::TOOL_MENTION_SIGIL;
use crate::skills::SkillMetadata;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::user_input::UserInput;
use chaos_syslog::SessionTelemetry;

#[derive(Debug, Default)]
pub(crate) struct SkillInjections {
    pub(crate) items: Vec<ResponseItem>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) async fn build_skill_injections(
    mentioned_skills: &[SkillMetadata],
    otel: Option<&SessionTelemetry>,
    analytics_client: &AnalyticsEventsClient,
    tracking: TrackEventsContext,
) -> SkillInjections {
    let _ = otel;
    let _ = analytics_client;
    let _ = tracking;

    let mut items = Vec::with_capacity(mentioned_skills.len());
    let mut warnings = Vec::new();

    for skill in mentioned_skills {
        match fs::read_to_string(&skill.path_to_skills_md) {
            Ok(contents) => items.push(
                SkillInstructions {
                    name: skill.name.clone(),
                    path: skill.path_to_skills_md.to_string_lossy().into_owned(),
                    contents: strip_frontmatter(&contents).to_string(),
                }
                .into(),
            ),
            Err(err) => warnings.push(format!(
                "Failed to load skill `{}` from {}: {err}",
                skill.name,
                skill.path_to_skills_md.display()
            )),
        }
    }

    SkillInjections { items, warnings }
}

pub(crate) fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
    disabled_paths: &HashSet<PathBuf>,
    connector_slug_counts: &HashMap<String, usize>,
) -> Vec<SkillMetadata> {
    let _ = connector_slug_counts;
    if skills.is_empty() {
        return Vec::new();
    }

    let messages = inputs
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut mentioned_names = HashSet::new();
    let mut mentioned_paths = HashSet::new();

    for input in inputs {
        match input {
            UserInput::Skill { name, path } => {
                mentioned_names.insert(name.clone());
                mentioned_paths.insert(normalize_path(path));
            }
            UserInput::Mention { name, path } => {
                if tool_kind_for_path(path) != ToolMentionKind::Skill {
                    continue;
                }
                mentioned_names.insert(name.clone());
                if let Some(skill_name) = skill_name_from_tool_path(path) {
                    mentioned_names.insert(skill_name.to_string());
                }
                if is_skill_filename(path) {
                    mentioned_paths.insert(normalize_string_path(path));
                }
            }
            UserInput::Text { .. }
            | UserInput::Image { .. }
            | UserInput::LocalImage { .. } => {}
            _ => {}
        }
    }

    for message in messages {
        let mentions = extract_tool_mentions_with_sigil(message, TOOL_MENTION_SIGIL);
        mentioned_names.extend(mentions.plain_names().map(str::to_string));
        for path in mentions.paths() {
            if tool_kind_for_path(path) != ToolMentionKind::Skill {
                continue;
            }
            if let Some(skill_name) = skill_name_from_tool_path(path) {
                mentioned_names.insert(skill_name.to_string());
            }
            if is_skill_filename(path) {
                mentioned_paths.insert(normalize_string_path(path));
            }
        }
    }

    skills
        .iter()
        .filter(|skill| !disabled_paths.contains(&skill.path_to_skills_md))
        .filter(|skill| {
            mentioned_names.contains(skill.name.as_str())
                || mentioned_paths.contains(&normalize_path(&skill.path_to_skills_md))
        })
        .cloned()
        .collect()
}

pub(crate) struct ToolMentions<'a> {
    paths: HashSet<&'a str>,
    plain_names: HashSet<&'a str>,
}

impl<'a> ToolMentions<'a> {
    pub(crate) fn plain_names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.plain_names.iter().copied()
    }

    pub(crate) fn paths(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.paths.iter().copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolMentionKind {
    App,
    Mcp,
    Plugin,
    Skill,
    Other,
}

pub(crate) const APP_PATH_PREFIX: &str = "app://";
pub(crate) const MCP_PATH_PREFIX: &str = "mcp://";
pub(crate) const PLUGIN_PATH_PREFIX: &str = "plugin://";
const SKILL_PATH_PREFIX: &str = "skill://";
const SKILL_FILENAME: &str = "SKILL.md";

pub(crate) fn tool_kind_for_path(path: &str) -> ToolMentionKind {
    if path.starts_with(APP_PATH_PREFIX) {
        ToolMentionKind::App
    } else if path.starts_with(MCP_PATH_PREFIX) {
        ToolMentionKind::Mcp
    } else if path.starts_with(PLUGIN_PATH_PREFIX) {
        ToolMentionKind::Plugin
    } else if path.starts_with(SKILL_PATH_PREFIX) || is_skill_filename(path) {
        ToolMentionKind::Skill
    } else {
        ToolMentionKind::Other
    }
}

fn normalize_path(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf())
}

fn normalize_string_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn skill_name_from_tool_path(path: &str) -> Option<&str> {
    path.strip_prefix(SKILL_PATH_PREFIX)
        .and_then(|value| value.rsplit('/').next())
        .filter(|value| !value.is_empty())
}

fn strip_frontmatter(contents: &str) -> &str {
    let Some(rest) = contents.strip_prefix("---\n") else {
        return contents.trim();
    };
    let Some((_, body)) = rest.split_once("\n---\n") else {
        return contents.trim();
    };
    body.trim()
}

fn is_skill_filename(path: &str) -> bool {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file_name.eq_ignore_ascii_case(SKILL_FILENAME)
}

#[allow(dead_code)]
pub(crate) fn app_id_from_path(path: &str) -> Option<&str> {
    path.strip_prefix(APP_PATH_PREFIX)
        .filter(|value| !value.is_empty())
}

pub(crate) fn plugin_config_name_from_path(path: &str) -> Option<&str> {
    path.strip_prefix(PLUGIN_PATH_PREFIX)
        .filter(|value| !value.is_empty())
}

pub(crate) fn extract_tool_mentions_with_sigil(text: &str, sigil: char) -> ToolMentions<'_> {
    let text_bytes = text.as_bytes();
    let mut mentioned_paths: HashSet<&str> = HashSet::new();
    let mut plain_names: HashSet<&str> = HashSet::new();

    let mut index = 0;
    while index < text_bytes.len() {
        let byte = text_bytes[index];
        if byte == b'['
            && let Some((name, path, end_index)) =
                parse_linked_tool_mention(text, text_bytes, index, sigil)
        {
            if !is_common_env_var(name) {
                mentioned_paths.insert(path);
            }
            index = end_index;
            continue;
        }

        if byte != sigil as u8 {
            index += 1;
            continue;
        }

        let name_start = index + 1;
        let Some(first_name_byte) = text_bytes.get(name_start) else {
            index += 1;
            continue;
        };
        if !is_mention_name_char(*first_name_byte) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while let Some(next_byte) = text_bytes.get(name_end)
            && is_mention_name_char(*next_byte)
        {
            name_end += 1;
        }

        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            plain_names.insert(name);
        }
        index = name_end;
    }

    ToolMentions {
        paths: mentioned_paths,
        plain_names,
    }
}

fn parse_linked_tool_mention<'a>(
    text: &'a str,
    text_bytes: &[u8],
    start: usize,
    sigil: char,
) -> Option<(&'a str, &'a str, usize)> {
    let sigil_index = start + 1;
    if text_bytes.get(sigil_index) != Some(&(sigil as u8)) {
        return None;
    }

    let name_start = sigil_index + 1;
    let first_name_byte = text_bytes.get(name_start)?;
    if !is_mention_name_char(*first_name_byte) {
        return None;
    }

    let mut name_end = name_start + 1;
    while let Some(next_byte) = text_bytes.get(name_end)
        && is_mention_name_char(*next_byte)
    {
        name_end += 1;
    }

    if text_bytes.get(name_end) != Some(&b']') {
        return None;
    }

    let mut path_start = name_end + 1;
    while let Some(next_byte) = text_bytes.get(path_start)
        && next_byte.is_ascii_whitespace()
    {
        path_start += 1;
    }
    if text_bytes.get(path_start) != Some(&b'(') {
        return None;
    }

    let mut path_end = path_start + 1;
    while let Some(next_byte) = text_bytes.get(path_end)
        && *next_byte != b')'
    {
        path_end += 1;
    }
    if text_bytes.get(path_end) != Some(&b')') {
        return None;
    }

    let path = text[path_start + 1..path_end].trim();
    if path.is_empty() {
        return None;
    }

    let name = &text[name_start..name_end];
    Some((name, path, path_end + 1))
}

fn is_common_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}
