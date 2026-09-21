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

For TOML editing, export your settings, edit the exported file, and import it:

```sh
chaos config export > settings.toml
# Edit settings.toml, preserving your other settings.
chaos config import settings.toml
```

Import replaces user settings, so edit the full export rather than importing
an appearance-only snippet. The appearance section looks like this:

```toml
[appearance.colors]
fg = "#dddddd"
accent = "#88c0d0"
border = "blue"

[appearance.user]
fg = "#a3be8c"
bold = true

[appearance.assistant]
fg = "#dddddd"
italic = false
```

Omitted settings preserve the existing appearance. The normal configuration
layering applies; for example, `-c 'appearance.user.bold=true'` overrides just
that setting. Restart the UI to apply settings changes. Resume/fork uses the
final loaded configuration; previously emitted terminal scrollback is not
recolored.

User settings live in the settings database. Do not put appearance preferences
in `$CHAOS_HOME/config.toml`, which is reserved for bootstrap settings. Existing
legacy TOML installations use the normal `chaos config migrate` workflow.
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

The activity eye (`◉`) follows the currently viewed agent. It breathes while
recent work is observed; only the eye's styling breathes, never the transcript
background or status text. Colors follow `accent`, `warning`, `error`, and `top_bar_dim`.
RGB accents breathe smoothly on true-color terminals; terminal-defined ANSI
colors use dim/normal/bold levels. Set `tui.animations=false` to keep it static:

```sh
chaos config set tui.animations false
```

Working, idle, and tool captions use system-seeded randomness, changing every
10–60 seconds without immediate repeats. Tool captions carry a `🔧` marker.
Phase and active-peer changes can select a different catalog; redraws and runtime
traffic do not reset the timer. Interrupted captions are selected once and stay
fixed until the phase changes.

Quiet, input, retry, and error signals take precedence over captions.
Starting, waiting, and closed/disconnected states keep their explicit labels.
Setting `tui.animations=false` restores literal labels and disables caption timers.
`/agent` always shows the literal phase.

After 30 seconds without observed work, it becomes steady warning-colored and
shows `No activity · Ns` (or `Tools · no activity Ns`). This means **quiet,
possibly stalled**, not proof of a frozen model: reasoning and tools can be
silent. Waiting on agents, operator input, reconnecting, errors, interruption,
and closed/disconnected event sources have distinct states. This is advisory;
it never cancels or retries work.

Each agent has its own activity clock, including background agents. `Others:`
summarizes the other tracked agents, including the parent when viewing a child.
A quiet or failed sibling makes the eye warning/error-colored even if the
viewed agent is busy. `/agent` shows per-agent state and the ages of its last
observed activity and runtime event, sampled when the picker opens. Switching
transcripts does not reset these clocks; replayed history is not live activity.
Estimated token progress may update the runtime-event age, but **never** renews
observed activity. Runtime traffic is not a provider/model heartbeat.

The eye has highest layout priority and remains when its labels no longer fit.
The eye stops breathing when idle, while captions use a random 10–60-second
timer. Hiding the top bar stops both; with animations disabled, idle captions
have no timer. Quiet-state detection continues with animations disabled.

The pinned row includes local time (`HH:MM`) and live `chaos-machine` observations:
form factor, headless/SSH hints, current power, relevant disk space, and thermals.
Headless describes display detection, not chassis; SSH does not imply headless.

Machine reads run off the drawing thread, approximately every 30 seconds and when
the active workspace/configuration changes. The clock updates at minute boundaries
even while a probe is slow. Hiding the row stops its polling. Failed/timed-out reads
clear old values and show a neutral `machine ?`, not a health claim.

The power indicator is hidden on machines identified as desktops; an absent
battery alone does not establish that classification. On other machines, power
shows AC without low-battery coloring for dead/removed batteries on external
power, or `power ?` when the source is unknown. System/UPS percentages stay
individual, separated by `/`.

Disk space is the lowest caller-available percentage among the active workspace,
state, and temporary filesystems, not all mounted drives or a combined capacity.
A trailing `?` means some relevant storage observations are unavailable.

CPU Celsius shows the highest observed **physical** CPU channel, not a package
average. Control/unknown-scale readings are never relabeled as physical Celsius.
OS thermal state and CPU threshold alerts take precedence over that numeric label;
unsupported thermal observations are hidden, not reported as zero or cool.

Warning colors use the same global `machine_warnings` policy as model checks (see
`chaos-mcp(7)`). Warnings get layout priority over routine readings and the clock;
whole widgets disappear when the terminal is too narrow. These indicators neither
notify the model continuously nor interrupt tools or save work automatically.

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
highlighting** colors; they do not replace these appearance overrides.

Unknown appearance keys and invalid colors are configuration errors.
Font families and sizes are controlled by the terminal; ChaOS only sets colors
and bold/italic attributes. There are no theme plugins, inheritance, or live
reload.
