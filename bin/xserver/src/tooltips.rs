use rand::Rng;

const RAW_TOOLTIPS: &str = include_str!("../tooltips.txt");

/// Pick a random tooltip to show to the user when starting Codex.
pub(crate) fn get_tooltip(
    _plan: Option<chaos_ipc::account::PlanType>,
    _fast_mode_enabled: bool,
) -> Option<String> {
    let tips: Vec<&str> = RAW_TOOLTIPS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();

    if tips.is_empty() {
        return None;
    }

    let mut rng = rand::rng();
    tips.get(rng.random_range(0..tips.len()))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_tooltip_returns_some_tip_when_available() {
        assert!(get_tooltip(None, false).is_some());
    }
}
