use crate::skills::model::SkillMetadata;
use chaos_ipc::protocol::SKILLS_INSTRUCTIONS_CLOSE_TAG;
use chaos_ipc::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;

pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines = vec![
        "## Skills".to_string(),
        "Skills are reusable local capabilities available in this session. Use them when they clearly match the task.".to_string(),
        "### Available skills".to_string(),
    ];

    lines.extend(skills.iter().map(|skill| {
        let description = skill
            .short_description
            .as_deref()
            .unwrap_or(&skill.description);
        let path = skill.path_to_skills_md.to_string_lossy().replace('\\', "/");
        if description.is_empty() {
            format!("- {} ({path})", skill.name)
        } else {
            format!("- {}: {description} ({path})", skill.name)
        }
    }));

    let body = lines.join("\n");
    Some(format!(
        "{SKILLS_INSTRUCTIONS_OPEN_TAG}\n{body}\n{SKILLS_INSTRUCTIONS_CLOSE_TAG}"
    ))
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
