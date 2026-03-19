use std::path::Path;

pub(crate) fn executable_lookup_key(raw: &str) -> String {
    raw.to_string()
}

pub(crate) fn executable_path_lookup_key(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(executable_lookup_key)
}
