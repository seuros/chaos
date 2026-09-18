use super::*;

#[test]
fn context_window_arg_accepts_documented_presets_and_aliases() {
    assert_eq!(
        parse_context_window_arg("catalog"),
        Ok(ContextWindowArg::Preset(ChatgptContextWindow::Catalog))
    );
    assert_eq!(
        parse_context_window_arg("default"),
        Ok(ContextWindowArg::Preset(ChatgptContextWindow::Catalog))
    );
    assert_eq!(
        parse_context_window_arg("observed-400k"),
        Ok(ContextWindowArg::Preset(ChatgptContextWindow::Observed400k))
    );
    assert_eq!(
        parse_context_window_arg("400K"),
        Ok(ContextWindowArg::Preset(ChatgptContextWindow::Observed400k))
    );
    assert_eq!(
        parse_context_window_arg("status"),
        Ok(ContextWindowArg::Status)
    );
}

#[test]
fn context_window_arg_rejects_unknown_preset() {
    assert_eq!(parse_context_window_arg("huge"), Err(()));
}
