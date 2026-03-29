use std::path::PathBuf;

use chaos_ipc::protocol::SkillScope;
use pretty_assertions::assert_eq;

use super::*;

fn skill(name: &str, description: &str, path: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: description.to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        permission_profile: None,
        managed_network_override: None,
        path_to_skills_md: PathBuf::from(path),
        scope: SkillScope::User,
    }
}

#[test]
fn render_skills_section_returns_none_for_empty_skills() {
    assert_eq!(render_skills_section(&[]), None);
}

#[test]
fn render_skills_section_includes_summary_and_path() {
    let rendered =
        render_skills_section(&[skill("demo", "build charts", "/tmp/skills/demo/SKILL.md")])
            .expect("skills section should render");

    let expected = "<skills_instructions>\n## Skills\nSkills are reusable local capabilities available in this session. Use them when they clearly match the task.\n### Available skills\n- demo: build charts (/tmp/skills/demo/SKILL.md)\n</skills_instructions>";

    assert_eq!(rendered, expected);
}
