use std::path::Path;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KnownShell {
    Zsh,
    Bash,
    Sh,
}

pub fn detect_shell_type(shell_path: &Path) -> Option<KnownShell> {
    match shell_path.as_os_str().to_str() {
        Some("zsh") => Some(KnownShell::Zsh),
        Some("sh") => Some(KnownShell::Sh),
        Some("bash") => Some(KnownShell::Bash),
        _ => {
            let shell_name = shell_path.file_stem();
            if let Some(shell_name) = shell_name {
                let shell_name_path = Path::new(shell_name);
                if shell_name_path != shell_path {
                    return detect_shell_type(shell_name_path);
                }
            }
            None
        }
    }
}
