use crossterm::event::KeyCode;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::key_hint;
use crate::key_hint::KeyBinding;

use super::types::ShortcutsState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShortcutId {
    Commands,
    ShellCommands,
    InsertNewline,
    QueueMessageTab,
    FilePaths,
    PasteImage,
    ExternalEditor,
    EditPrevious,
    Quit,
    ShowTranscript,
    ChangeMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ShortcutBinding {
    pub key: KeyBinding,
    pub condition: DisplayCondition,
}

impl ShortcutBinding {
    pub(super) fn matches(&self, state: ShortcutsState) -> bool {
        self.condition.matches(state)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DisplayCondition {
    Always,
    WhenShiftEnterHint,
    WhenNotShiftEnterHint,
    WhenCollaborationModesEnabled,
}

impl DisplayCondition {
    pub(super) fn matches(self, state: ShortcutsState) -> bool {
        match self {
            DisplayCondition::Always => true,
            DisplayCondition::WhenShiftEnterHint => state.use_shift_enter_hint,
            DisplayCondition::WhenNotShiftEnterHint => !state.use_shift_enter_hint,
            DisplayCondition::WhenCollaborationModesEnabled => state.collaboration_modes_enabled,
        }
    }
}

pub(super) struct ShortcutDescriptor {
    pub id: ShortcutId,
    pub bindings: &'static [ShortcutBinding],
    pub prefix: &'static str,
    pub label: &'static str,
}

impl ShortcutDescriptor {
    pub(super) fn binding_for(&self, state: ShortcutsState) -> Option<&'static ShortcutBinding> {
        self.bindings.iter().find(|binding| binding.matches(state))
    }

    pub(super) fn overlay_entry(&self, state: ShortcutsState) -> Option<Line<'static>> {
        let binding = self.binding_for(state)?;
        let mut line = Line::from(vec![self.prefix.into(), binding.key.into()]);
        match self.id {
            ShortcutId::EditPrevious => {
                if state.esc_backtrack_hint {
                    line.push_span(" again to edit previous message");
                } else {
                    line.extend(vec![
                        " ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to edit previous message".into(),
                    ]);
                }
            }
            _ => line.push_span(self.label),
        };
        Some(line)
    }
}

pub(super) const SHORTCUTS: &[ShortcutDescriptor] = &[
    ShortcutDescriptor {
        id: ShortcutId::Commands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('/')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShellCommands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('!')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for shell commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::InsertNewline,
        bindings: &[
            ShortcutBinding {
                key: key_hint::shift(KeyCode::Enter),
                condition: DisplayCondition::WhenShiftEnterHint,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('j')),
                condition: DisplayCondition::WhenNotShiftEnterHint,
            },
        ],
        prefix: "",
        label: " for newline",
    },
    ShortcutDescriptor {
        id: ShortcutId::QueueMessageTab,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Tab),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to queue message",
    },
    ShortcutDescriptor {
        id: ShortcutId::FilePaths,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('@')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for file paths",
    },
    ShortcutDescriptor {
        id: ShortcutId::PasteImage,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('v')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to paste images",
    },
    ShortcutDescriptor {
        id: ShortcutId::ExternalEditor,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('g')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to edit in external editor",
    },
    ShortcutDescriptor {
        id: ShortcutId::EditPrevious,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Esc),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: "",
    },
    ShortcutDescriptor {
        id: ShortcutId::Quit,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('c')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to exit",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShowTranscript,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('t')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to view transcript",
    },
    ShortcutDescriptor {
        id: ShortcutId::ChangeMode,
        bindings: &[ShortcutBinding {
            key: key_hint::shift(KeyCode::Tab),
            condition: DisplayCondition::WhenCollaborationModesEnabled,
        }],
        prefix: "",
        label: " to change mode",
    },
];

pub(super) fn shortcut_overlay_lines(state: ShortcutsState) -> Vec<Line<'static>> {
    let mut commands = Line::from("");
    let mut shell_commands = Line::from("");
    let mut newline = Line::from("");
    let mut queue_message_tab = Line::from("");
    let mut file_paths = Line::from("");
    let mut paste_image = Line::from("");
    let mut external_editor = Line::from("");
    let mut edit_previous = Line::from("");
    let mut quit = Line::from("");
    let mut show_transcript = Line::from("");
    let mut change_mode = Line::from("");

    for descriptor in SHORTCUTS {
        if let Some(text) = descriptor.overlay_entry(state) {
            match descriptor.id {
                ShortcutId::Commands => commands = text,
                ShortcutId::ShellCommands => shell_commands = text,
                ShortcutId::InsertNewline => newline = text,
                ShortcutId::QueueMessageTab => queue_message_tab = text,
                ShortcutId::FilePaths => file_paths = text,
                ShortcutId::PasteImage => paste_image = text,
                ShortcutId::ExternalEditor => external_editor = text,
                ShortcutId::EditPrevious => edit_previous = text,
                ShortcutId::Quit => quit = text,
                ShortcutId::ShowTranscript => show_transcript = text,
                ShortcutId::ChangeMode => change_mode = text,
            }
        }
    }

    let mut ordered = vec![
        commands,
        shell_commands,
        newline,
        queue_message_tab,
        file_paths,
        paste_image,
        external_editor,
        edit_previous,
        quit,
    ];
    if change_mode.width() > 0 {
        ordered.push(change_mode);
    }
    ordered.push(Line::from(""));
    ordered.push(show_transcript);

    build_columns(ordered)
}

fn build_columns(entries: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return Vec::new();
    }

    const COLUMNS: usize = 2;
    const COLUMN_PADDING: [usize; COLUMNS] = [4, 4];
    const COLUMN_GAP: usize = 4;

    let rows = entries.len().div_ceil(COLUMNS);
    let target_len = rows * COLUMNS;
    let mut entries = entries;
    if entries.len() < target_len {
        entries.extend(std::iter::repeat_n(
            Line::from(""),
            target_len - entries.len(),
        ));
    }

    let mut column_widths = [0usize; COLUMNS];

    for (idx, entry) in entries.iter().enumerate() {
        let column = idx % COLUMNS;
        column_widths[column] = column_widths[column].max(entry.width());
    }

    for (idx, width) in column_widths.iter_mut().enumerate() {
        *width += COLUMN_PADDING[idx];
    }

    entries
        .chunks(COLUMNS)
        .map(|chunk| {
            let mut line = Line::from("");
            for (col, entry) in chunk.iter().enumerate() {
                line.extend(entry.spans.clone());
                if col < COLUMNS - 1 {
                    let target_width = column_widths[col];
                    let padding = target_width.saturating_sub(entry.width()) + COLUMN_GAP;
                    line.push_span(Span::from(" ".repeat(padding)));
                }
            }
            line.dim()
        })
        .collect()
}
