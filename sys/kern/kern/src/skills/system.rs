use std::path::Path;
use std::path::PathBuf;

const SYSTEM_SKILLS: &[(&str, &str)] = &[(
    "skill-creator",
    include_str!("builtins/skill-creator/SKILL.md"),
)];

pub(crate) fn system_cache_root_dir(chaos_home: &Path) -> PathBuf {
    chaos_home.join("skills").join(".system")
}

pub(crate) fn install_system_skills(chaos_home: &Path) -> std::io::Result<()> {
    let root = system_cache_root_dir(chaos_home);
    std::fs::create_dir_all(&root)?;

    for (name, contents) in SYSTEM_SKILLS {
        let skill_dir = root.join(name);
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(skill_dir.join("SKILL.md"), contents)?;
    }

    Ok(())
}

pub(crate) fn uninstall_system_skills(chaos_home: &Path) {
    let _ = std::fs::remove_dir_all(system_cache_root_dir(chaos_home));
}
