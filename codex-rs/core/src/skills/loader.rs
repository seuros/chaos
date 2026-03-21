use std::path::PathBuf;

use codex_protocol::protocol::SkillScope;

use crate::skills::model::SkillLoadOutcome;

pub(crate) struct SkillRoot {
    pub(crate) path: PathBuf,
    pub(crate) scope: SkillScope,
}

pub(crate) fn load_skills_from_roots<I>(_roots: I) -> SkillLoadOutcome
where
    I: IntoIterator<Item = SkillRoot>,
{
    SkillLoadOutcome::default()
}
