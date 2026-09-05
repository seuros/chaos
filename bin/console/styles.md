# Headers, primary, and secondary text

- **Headers:** Use `bold`. For markdown with various header levels, leave in the `#` signs.
- **Primary text:** Default.
- **Secondary text:** Use `dim`.

# Foreground colors

- **Default:** Most of the time, just use the default foreground color. `reset` can help get it back.
- **User input tips, selection, and status indicators:** Use ANSI `cyan`.
- **Success and additions:** Use ANSI `green`.
- **Errors, failures and deletions:** Use ANSI `red`.
- **Chaos:** Use ANSI `magenta`.

# Avoid

- Avoid custom colors because there's no guarantee that they'll contrast well or look good in various terminal color themes. (`shimmer.rs` is an exception that works well because we take the default colors and just adjust their levels.)
- Avoid ANSI `black` & `white` as foreground colors because the default terminal theme color will do a better job. (Use `reset` if you need to in order to get those.) The exception is if you need contrast rendering over a manually colored background.
- Avoid ANSI `blue` and `yellow` because for now the style guide doesn't use them. Prefer a foreground color mentioned above.

# Top-bar widgets

The top bar is a full-width, single-row container of independent Rust widgets.
Left-to-right order within each side is independent of visibility priority:

| Side | Widget | Priority |
|---|---|---:|
| Left | Hostname | 180 |
| Left | OS/distro | 100 |
| Left | Architecture | 80 |
| Left | Sandbox mechanism | 160 |
| Left | Container/jail | 150 |
| Left | Multiplexer and stable ID | 120 |
| Right | Storage backend | 200 |
| Right | Persistence warning | 255 |
| Right | Battery | 240 |
| Right | Clock | 220 |

- Keep each built-in widget in its own file under `lib/libui/top_bar/widgets/`.
- Declare its identity, side, and `u8` priority. The shared widget measures the
  complete label in terminal columns, including wide and combining characters. Higher
  priorities stay visible; ties keep declaration order. Hide the entire widget
  when it does not fit, rather than truncate, wrap, or overlap.
- The container owns the full-row background, one-column outer margins, and
  separators between visible widgets. Render only inside the assigned rectangle.
- Keep rendering pure. The shared runtime refreshes cached state and schedules
  redraws on changes; do not perform I/O or create timers inside rendering.
- The clock refreshes on wall-clock minute boundaries, including while hidden.
  Disabling the bar or dropping the UI cancels its updater.
- The hostname is collected once. Power is read immediately and every 30 seconds
  on a background worker, never under the rendering lock. No overlapping reads
  or catch-up bursts; on macOS each `pmset` read has a two-second deadline.
  A completed snapshot updates even a hidden battery widget. Only changed state
  requests a redraw. Machines without a detected battery omit the widget;
  an unknown charge level is `?%`, not a fabricated `0%`.
- The other static environment widgets load once from `chaos_sysinfo` on a
  background worker. They appear when ready without blocking the clock, power
  monitoring, or drawing. Missing sandbox/container/multiplexer labels are omitted.
  The sandbox label names the platform mechanism, not the current permission policy.
- Storage (`SQLITE` or `🐘`) and persistence (`⚠ log` only when unhealthy) consume
  retained kernel status notifications, including while hidden or idle. There is
  no persistence polling timer. Degraded health uses the warning color; failing
  or failed health uses the error color. Recovery removes the warning and its
  separator. Dropping the bar releases its status subscription.

## Adding a widget

`lib/libui/top_bar/widget.rs` provides the common cached-text implementation:

- `BarWidget::text`: static `Content`, with no update work.
- `BarWidget::timed`: a pure wall-time sampler returning `Content` and the delay
  until its next refresh; unchanged content does not request a redraw.
- `BarWidget::watched`: a retained watch snapshot, a field selector, and a
  presentation function. Only changes to the selected state rebuild content.
  The runtime owns source notifications or polling, not the widget.

Use `Content::new(label)` with semantic `Tone` and optional `.bold()`. Empty
content hides the widget. Width measurement and Ratatui rendering are shared;
tones resolve against the container's palette at draw time, not a cached color.
Keep widget files limited to metadata and presentation rules, for example:

```rust
use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(name: String) -> BarWidget {
    BarWidget::text("hostname", Side::Left, 180, Content::new(name).bold())
}
```

Register factories in `lib/libui/top_bar/widgets.rs`: immediately available
widgets in `initial_widgets`, background-loaded environment widgets in
`environment_widgets`. Both preserve declaration order within each side.
Keep all blocking collection and wakeup ownership in `runtime.rs`; refresh
callbacks must remain nonblocking, even when their widget is hidden.

(There are some rules to try to catch this in `clippy.toml`.)
