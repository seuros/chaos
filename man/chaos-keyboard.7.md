# chaos-keyboard(7)

## NAME

chaos-keyboard - keyboard shortcuts for the FreeChaOS terminal UI

## DESCRIPTION

Press `?` with an empty composer (message input) to toggle the in-app shortcut
help. This page is the fuller reference, including editing, navigation, and
terminal-specific bindings.

Shortcuts are context-sensitive. Full-screen overlays handle their own keys.
Composer shortcuts apply when the chat pane is focused; popups and dialogs may
handle them differently. Unless noted otherwise, the global shortcuts below work
from any focused pane when no full-screen overlay is open.

On macOS, `Alt` is usually the Option key, displayed as `⌥` in the UI. Your
terminal must send it as Alt/Meta rather than inserting special characters.

## GLOBAL SHORTCUTS

| Shortcut | Action / conditions |
| --- | --- |
| `Ctrl+O` | Open the full-screen session log viewer; no popup or modal may be active. |
| `Ctrl+T` | Open the transcript viewer. |
| `PageUp` / `PageDown` | Open the transcript and page up/down; no popup or modal may be active. |
| `Home` / `End` | Open the transcript at its beginning/end; no popup or modal may be active. |
| `Ctrl+L` | Clear the terminal display and in-memory transcript view while idle. Does not start a new session or clear model context. |
| `Ctrl+G` | Edit the draft in an external editor; no popup or modal may be active. Uses `VISUAL`, then `EDITOR`. |

`Home` and `End` are transcript-navigation keys in the main UI, not composer
line-motion keys. Use `Ctrl+A` and `Ctrl+E` to move within the draft.

## COMPOSER AND SESSION

| Shortcut | Action / conditions |
| --- | --- |
| `?` | Toggle shortcut help with an empty composer. Typing in a draft inserts `?` normally. |
| `Enter` | Submit the message. During a running turn, submit it as a steer. |
| `Tab` | Queue the message during a running turn; submit normally while idle. In a completion popup, complete the selected item instead. |
| `Shift+Enter` / `Ctrl+J` | Insert a newline. Use `Ctrl+J` if the terminal cannot distinguish `Shift+Enter`. |
| `/` | Start a slash command. |
| `!` | Start a shell command. |
| `@` | Search for a file path to insert. |
| `Ctrl+V` / `Alt+V` | Attach an image from the clipboard. Use the terminal's normal paste shortcut for text. |
| `Up` / `Down`, `Ctrl+P` / `Ctrl+N` | Recall input history from an empty composer, or at the start/end of an unchanged recalled entry; otherwise move the cursor vertically. In popups, move the selection. |
| `Alt+Up` | Restore the latest queued message for editing. Apple Terminal, Warp, and VS Code use `Shift+Left` instead. |
| `Shift+Tab` | Cycle available collaboration modes while idle, with no popup or modal. |
| `Esc` | Dismiss/cancel the active view or interrupt a running turn. With an idle, empty composer, prime editing of a previous message. |
| `Esc`, `Esc` | Preview the previous user message for editing while idle with an empty composer; `Enter` confirms. |
| `Ctrl+C` | Cancel the active view or clear a draft first; otherwise interrupt active work, or quit when idle. |
| `Ctrl+D` | Quit with an empty composer and no popup or modal; otherwise delete the character under the cursor. |

In previous-message preview, `Esc` / `Left` moves to an older message,
`Right` moves forward, and `Enter` selects the message for editing and requests
a conversation rollback to that point. Later turns are removed after the
backend confirms the rollback.

## TEXT EDITING

These bindings apply in the composer, not in full-screen viewers.

| Shortcut | Action |
| --- | --- |
| `Left` / `Right`, `Ctrl+B` / `Ctrl+F` | Move one character left/right. |
| `Ctrl+A` / `Ctrl+E` | Move to the beginning/end of the line; repeat at that boundary to move to the adjacent line. |
| `Ctrl+Left` / `Ctrl+Right` | Move to the previous/next word boundary. |
| `Alt+Left` / `Alt+Right`, `Alt+B` / `Alt+F` | Move by word in a non-empty draft; see agent-switching exceptions below. |
| `Backspace` / `Ctrl+H` | Delete the preceding character. |
| `Delete` / `Ctrl+D` | Delete the following character. |
| `Ctrl+W` / `Alt+Backspace` / `Ctrl+Alt+H` | Delete the previous word. |
| `Alt+D` / `Alt+Delete` | Delete the next word. |
| `Ctrl+U` / `Ctrl+K` | Cut to the beginning/end of the line. |
| `Ctrl+Y` | Yank (restore) the most recently killed text. |

## AGENT SWITCHING

| Shortcut | Action |
| --- | --- |
| `Alt+Left` | Watch the previous agent. |
| `Alt+Right` | Watch the next agent. |

Switching requires an empty draft and no popup, modal, or full-screen overlay.
It cycles through known agents in first-seen spawn order, wrapping at either end.
Closed agents remain available for review. `/agent` opens the picker.

On macOS without enhanced keyboard reporting, `Alt+B` / `Alt+F` also act as
previous/next-agent shortcuts when the draft is empty, because some terminals
send those sequences for Option+Left/Right. A non-empty draft keeps word motion.

## PANE MANAGEMENT

These bindings are active only when multiple panes are open and no popup, modal,
or full-screen overlay is active. They take precedence over composer editing.

| Shortcut | Action |
| --- | --- |
| `Alt+H` / `Alt+J` / `Alt+K` / `Alt+L` | Focus the pane to the left / below / above / right (no Shift). |
| `Alt+Enter` | Cycle pane focus. |
| `Alt+Shift+H` / `Alt+Shift+K` | Resize the focused pane with a negative step (`-0.05`). |
| `Alt+Shift+J` / `Alt+Shift+L` | Resize the focused pane with a positive step (`+0.05`). |
| `Alt+Q` | Close the focused auxiliary pane; if chat is focused, close the last auxiliary pane opened. |
| `Alt+W` | Close all auxiliary panes and return to chat only. |
| `Esc` | Close the focused auxiliary pane. |

Pane-local keys belong to the focused pane and do not type into the chat draft.

## TRANSCRIPT AND LOG VIEWERS

| Shortcut | Action |
| --- | --- |
| `Up` / `K`, `Down` / `J` | Scroll one line up/down. |
| `PageUp` / `Shift+Space` / `Ctrl+B` | Scroll one page up. |
| `PageDown` / `Space` / `Ctrl+F` | Scroll one page down. |
| `Ctrl+U` / `Ctrl+D` | Scroll half a page up/down. |
| `Home` / `End` | Jump to the beginning/end. |
| `Q` / `Ctrl+C` | Close the viewer, without quitting the session. |
| `Ctrl+T` | Close the transcript viewer (not the log viewer). |
| `Esc` | Start previous-message preview in the transcript viewer; it does not close the log viewer. |

Letter keys in these tables do not require Shift unless explicitly shown.

## TERMINAL NOTES

- Enhanced keyboard reporting allows the UI to distinguish more modified keys,
  including `Shift+Enter`. Availability depends on the terminal.
- The queued-message edit hint uses the terminal-specific binding described above.
- On Unix, `Ctrl+Z` suspends the TUI; use the shell's `fg` command to resume it.
- Dialogs, approval prompts, and plugin panes show their own contextual key hints.

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md) — logging and terminal troubleshooting
- [chaos-modes.7](./chaos-modes.7.md) — mode discovery and switching
