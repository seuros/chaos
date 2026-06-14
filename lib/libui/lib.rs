#![warn(clippy::all)]

pub mod app_event;
pub mod app_event_sender;
pub mod bottom_pane;
pub mod chatwidget;
pub mod clipboard_paste;
pub mod clipboard_text;
pub mod collaboration_modes;
pub mod color;
pub mod custom_terminal;
pub mod debug_config;
pub mod diff_render;
pub mod exec_cell;
pub mod exec_command;
pub mod get_git_diff;
pub mod history_cell;
pub mod insert_history;
pub mod key_hint;
pub mod line_truncation;
pub mod live_wrap;
pub mod markdown;
pub mod markdown_render;
pub mod markdown_stream;
pub mod mention_codec;
pub mod multi_agents;
pub mod notifications;
pub mod osc8;
pub mod render;
pub mod session_log;
pub mod shimmer;
pub mod slash_command;
pub mod status;
pub mod status_indicator_widget;
pub mod streaming;
pub mod style;
pub mod terminal_palette;
pub mod text_formatting;
pub mod theme;
pub mod theme_picker;
pub mod tool_badges;
pub mod top_bar;
pub mod tui;
pub mod ui_consts;
pub mod version;
pub mod wrapping;

pub mod test_render;
pub mod test_support;

#[cfg(feature = "vt100-tests")]
pub mod test_backend;

#[cfg(test)]
mod tests {
    use std::future::Future;

    fn run_async(future: impl Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build libui test runtime")
            .block_on(future);
    }

    #[test]
    fn libui_suite() {
        std::thread::Builder::new()
            .name("libui-suite".to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(run_libui_suite)
            .expect("spawn libui suite thread")
            .join()
            .expect("libui suite panicked");
    }

    fn run_libui_suite() {
        crate::bottom_pane::tests::bottom_pane_suite();
        run_async(crate::chatwidget::tests::chatwidget_suite());
        crate::clipboard_paste::pasted_paths_tests::clipboard_paste_suite();
        crate::clipboard_text::tests::clipboard_text_suite();
        crate::custom_terminal::tests::custom_terminal_suite();
        crate::debug_config::tests::debug_config_suite();
        crate::diff_render::tests::diff_render_suite();
        crate::exec_cell::tests::exec_cell_suite();
        crate::exec_command::tests::exec_command_suite();
        run_async(crate::history_cell::tests::history_cell_suite());
        crate::insert_history::tests::insert_history_suite();
        crate::live_wrap::tests::live_wrap_suite();
        crate::markdown::tests::markdown_suite();
        crate::markdown_render::tests::markdown_render_suite();
        run_async(crate::markdown_stream::tests::markdown_stream_suite());
        crate::mention_codec::tests::mention_codec_suite();
        crate::multi_agents::tests::multi_agents_suite();
        crate::notifications::tests::notifications_suite();
        crate::osc8::tests::osc8_suite();
        crate::slash_command::tests::slash_command_suite();
        run_async(crate::status::tests::status_tests_suite());
        crate::status_indicator_widget::tests::status_indicator_widget_suite();
        run_async(crate::streaming::tests::streaming_suite());
        crate::text_formatting::tests::text_formatting_suite();
        crate::theme_picker::tests::theme_picker_suite();
        crate::tool_badges::tests::tool_badges_suite();
        crate::top_bar::tests::top_bar_suite();
        crate::tui::tests::tui_suite();
        crate::wrapping::tests::wrapping_suite();
    }
}
