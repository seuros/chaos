use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ShellType {
    Zsh,
    Bash,
    Sh,
}

pub(crate) fn detect_shell_type(shell_path: &PathBuf) -> Option<ShellType> {
    match shell_path.as_os_str().to_str() {
        Some("zsh") => Some(ShellType::Zsh),
        Some("sh") => Some(ShellType::Sh),
        Some("bash") => Some(ShellType::Bash),
        _ => {
            let shell_name = shell_path.file_stem();
            if let Some(shell_name) = shell_name {
                let shell_name_path = Path::new(shell_name);
                if shell_name_path != Path::new(shell_path) {
                    return detect_shell_type(&shell_name_path.to_path_buf());
                }
            }
            None
        }
    }
}
