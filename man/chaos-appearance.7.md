+++
title = "chaos-appearance(7)"
summary = "Configure terminal colors and user/assistant text styles."
+++

# chaos-appearance(7) — terminal appearance

## Configuration

Set optional overrides using the existing user-settings commands (values are JSON):

```sh
chaos config set appearance.colors.accent '"#88c0d0"'
chaos config set appearance.user.fg '"#a3be8c"'
chaos config set appearance.user.bold true
chaos config set appearance.assistant.italic false
```

To edit several settings at once, export your settings as JSON, edit the
exported file, and import it:

```sh
chaos config export > settings.json
# Edit settings.json, preserving your other settings.
chaos config import settings.json
```

Import replaces user settings, so edit the full export rather than importing
an appearance-only snippet. The appearance section looks like this:

```json
{
  "appearance": {
    "colors": { "fg": "#dddddd", "accent": "#88c0d0", "border": "blue" },
    "user": { "fg": "#a3be8c", "bold": true },
    "assistant": { "fg": "#dddddd", "italic": false }
  }
}
```

Omitted settings preserve the existing appearance. The normal configuration
layering applies; for example, `-c 'appearance.user.bold=true'` overrides just
that setting. Restart the UI to apply settings changes. Resume/fork uses the
final loaded configuration; previously emitted terminal scrollback is not
recolored.

User settings live in the settings database. The bootstrap file
`$CHAOS_HOME/config.toml` accepts only `storage_url` and `egress_url`.
Use `chaos config unset appearance.user.bold` to restore an individual default,
or `chaos config unset appearance` to remove all appearance overrides.

## Colors

`appearance.colors` accepts the existing semantic palette slots:

`bg`, `fg`, `dim`, `highlight`, `top_bar_bg`, `top_bar_fg`, `top_bar_dim`,
`user_msg_bg`, `border`, `warning`, `error`, `success`, `accent`,
`secondary_accent`, and `tertiary_accent`.

Values are six-digit `#RRGGBB` strings, `default` (the terminal's default color),
or one of these lowercase ANSI names:

`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`,
`dark_gray`, `light_red`, `light_green`, `light_yellow`, `light_blue`,
`light_magenta`, `light_cyan`, `white`.

Explicit overrides take precedence over built-in mode colors. Slots without
overrides continue to follow the current mode. ANSI colors and RGB fidelity
depend on the terminal's palette and capabilities.

## Top bar

The activity eye (`◉`) follows the viewed agent and breathes during recent work.
Set `tui.animations=false` for a static eye and literal status labels:

```sh
chaos config set tui.animations false
```

After 30 seconds without observed work, `No activity · Ns` means **quiet,
possibly stalled**, not frozen. Silent reasoning/tools may still be running;
the indicator never cancels or retries work. `/agent` shows literal per-agent
states. `Others:` includes background agents; a quiet or failed sibling can
change the eye's warning color even while the viewed agent is busy.

The pinned row shows local time, power, relevant disk space, and thermals,
refreshed approximately every 30 seconds. `?` means unavailable or incomplete,
not healthy. Disk space is the lowest available percentage across workspace,
state, and temporary filesystems, not all drives. CPU Celsius is the highest
physical channel, not an average; macOS may show thermal pressure instead.
Headless/SSH hints do not identify the chassis.

Narrow terminals hide lower-priority widgets. Warning colors follow
`machine_warnings`; see [chaos-mcp(7)](./chaos-mcp.7.md).
UI readings do not continuously notify the model, interrupt tools, or save work.

## Message styles

Both `appearance.user` and `appearance.assistant` accept `fg`, `bg`, `bold`,
and `italic`. Role colors override the palette's message defaults.

User styles apply only to submitted messages, not the input composer or
selection popups. Assistant styles apply to prose, including streamed replies,
reasoning summaries, and proposed plans. Reasoning retains its dim/italic
distinction. Background overrides on assistant prose cover its text spans;
they do not fill the entire terminal row.

Markdown's explicit formatting takes precedence over base message styles.
For example, `bold = false` does not disable `**strong emphasis**`. Inline and
block code remain independent of role overrides. Markers and tool output keep
their own styles, using shared palette colors where applicable.

The existing `[tui] theme` setting and `/theme` picker still select **syntax
highlighting** colors; they do not replace these appearance overrides. Custom
syntax themes are `.tmTheme` documents stored in the runtime database:
`chaos themes import <file.tmTheme>` makes the file stem selectable in `/theme`;
`chaos themes list|show|export|remove|enable|disable` manage them.
`$CHAOS_HOME/themes/` is no longer read.

Unknown appearance keys and invalid colors are configuration errors.
Font families and sizes are controlled by the terminal; ChaOS only sets colors
and bold/italic attributes. There are no theme plugins, inheritance, or live
reload.
