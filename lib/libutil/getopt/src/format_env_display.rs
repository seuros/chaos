use std::collections::HashMap;

pub fn format_env_display(env: Option<&HashMap<String, String>>, env_vars: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(map) = env {
        let mut pairs: Vec<_> = map.iter().collect();
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        parts.extend(pairs.into_iter().map(|(key, _)| format!("{key}=*****")));
    }

    if !env_vars.is_empty() {
        parts.extend(env_vars.iter().map(|var| format!("{var}=*****")));
    }

    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(", ")
    }
}
