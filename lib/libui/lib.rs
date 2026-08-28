#![warn(clippy::all)]
#![cfg_attr(
    all(test, target_os = "macos"),
    allow(
        linker_messages,
        reason = "the monolithic libui test binary exceeds ld64's 16 MiB compact-unwind table"
    )
)]

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
mod modifier_diff;
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
mod table_detect;
pub mod terminal_palette;
pub mod text_formatting;
pub mod theme;
pub mod theme_picker;
pub mod tool_badges;
pub mod top_bar;
pub mod transcript_reflow;
pub mod tui;
pub mod ui_consts;
pub mod version;
pub mod width;
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

    /// Widget trees recurse deeper than the default 2 MiB test stack allows,
    /// so every suite runs on a thread of its own with room to work.
    ///
    /// Theme selection, the OSC 8 registry, and the palette cache are all
    /// process-global, so the suites hold a lock while they run. A runner that
    /// gives each test its own process still gets full parallelism; an
    /// in-process `cargo test` keeps the serial behaviour these suites were
    /// written under.
    fn run_suite(name: &'static str, suite: impl FnOnce() + Send + 'static) {
        static SEQUENCE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _sequence = SEQUENCE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::thread::Builder::new()
            .name(name.to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(suite)
            .expect("spawn libui suite thread")
            .join()
            .expect("libui suite panicked");
    }

    macro_rules! libui_suites {
        ($($name:ident => $suite:expr;)*) => {
            $(
                #[test]
                fn $name() {
                    run_suite(stringify!($name), || $suite);
                }
            )*
        };
    }

    libui_suites! {
        bottom_pane_suite => crate::bottom_pane::tests::bottom_pane_suite();
        chat_composer_input_suite => crate::bottom_pane::tests::chat_composer_input_suite();
        chat_composer_slash_suite => crate::bottom_pane::tests::chat_composer_slash_suite();
        chat_composer_paste_suite => crate::bottom_pane::tests::chat_composer_paste_suite();
        chat_composer_prompt_suite => crate::bottom_pane::tests::chat_composer_prompt_suite();
        chatwidget_suite => run_async(crate::chatwidget::tests::chatwidget_suite());
        clipboard_paste_suite => crate::clipboard_paste::pasted_paths_tests::clipboard_paste_suite();
        clipboard_text_suite => crate::clipboard_text::tests::clipboard_text_suite();
        custom_terminal_suite => crate::custom_terminal::tests::custom_terminal_suite();
        debug_config_suite => crate::debug_config::tests::debug_config_suite();
        diff_render_suite => crate::diff_render::tests::diff_render_suite();
        exec_cell_suite => crate::exec_cell::tests::exec_cell_suite();
        exec_command_suite => crate::exec_command::tests::exec_command_suite();
        history_cell_suite => run_async(crate::history_cell::tests::history_cell_suite());
        insert_history_suite => crate::insert_history::tests::insert_history_suite();
        live_wrap_suite => crate::live_wrap::tests::live_wrap_suite();
        markdown_suite => crate::markdown::tests::markdown_suite();
        markdown_render_suite => crate::markdown_render::tests::markdown_render_suite();
        markdown_stream_suite => run_async(crate::markdown_stream::tests::markdown_stream_suite());
        mention_codec_suite => crate::mention_codec::tests::mention_codec_suite();
        multi_agents_suite => crate::multi_agents::tests::multi_agents_suite();
        notifications_suite => crate::notifications::tests::notifications_suite();
        osc8_suite => crate::osc8::tests::osc8_suite();
        slash_command_suite => crate::slash_command::tests::slash_command_suite();
        status_suite => run_async(crate::status::tests::status_tests_suite());
        status_indicator_widget_suite => crate::status_indicator_widget::tests::status_indicator_widget_suite();
        streaming_suite => run_async(crate::streaming::tests::streaming_suite());
        table_detect_suite => crate::table_detect::tests::table_detect_suite();
        text_formatting_suite => crate::text_formatting::tests::text_formatting_suite();
        theme_picker_suite => crate::theme_picker::tests::theme_picker_suite();
        tool_badges_suite => crate::tool_badges::tests::tool_badges_suite();
        top_bar_suite => crate::top_bar::tests::top_bar_suite();
        transcript_reflow_suite => crate::transcript_reflow::tests::transcript_reflow_suite();
        tui_suite => crate::tui::tests::tui_suite();
        width_suite => crate::width::tests::width_suite();
        wrapping_suite => crate::wrapping::tests::wrapping_suite();
    }
}
