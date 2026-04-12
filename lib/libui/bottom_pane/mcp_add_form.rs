//! Multi-field form overlay for adding a new MCP server entry.
//!
//! The form collects four fields — name, command/url, args, env/headers — and
//! on confirmation writes them to the project `.mcp.json` file and asks the
//! kernel to hot-reload its MCP server registry.
//!
//! Navigation:
//!   Tab / Enter   → advance to next field (or submit on last)
//!   Shift+Tab     → go back to previous field
//!   Esc           → cancel and dismiss
//!
//! # `.mcp.json` format written
//! ```json
//! {
//!   "mcpServers": {
//!     "<name>": {
//!       "command": "<command>",
//!       "args": ["<arg1>", ...],
//!       "env": { "KEY": "VALUE", ... }
//!     }
//!   }
//! }
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::textarea::TextArea;
use crate::bottom_pane::textarea::TextAreaState;
use crate::render::renderable::Renderable;
use chaos_ipc::ProcessId;
use chaos_kern::McpAddServerParams;
use chaos_kern::add_server_to_dot_mcp_json;
use chaos_kern::config_loader::ConfigLayerStack;
use chaos_kern::project_mcp_json_path_for_cwd;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;

/// Number of form fields.
const FIELD_COUNT: usize = 4;

/// Field index constants.
const FIELD_NAME: usize = 0;
const FIELD_COMMAND: usize = 1;
const FIELD_ARGS: usize = 2;
const FIELD_ENV: usize = 3;

static FIELD_LABELS: [&str; FIELD_COUNT] = ["name", "command / url", "args", "env / headers"];
static FIELD_PLACEHOLDERS: [&str; FIELD_COUNT] = [
    "required — server key in mcpServers",
    "required — binary path or https://... URL",
    "optional — shell-style args; quote values with spaces (stdio only)",
    "optional — KEY=VALUE,... env vars (stdio) or headers / ENV_VAR_NAME (http)",
];

/// Bottom-pane overlay that collects MCP server details from the user.
pub struct McpAddForm {
    /// Working directory — used to locate the project `.mcp.json`.
    cwd: PathBuf,
    config_layer_stack: ConfigLayerStack,
    process_id: Option<ProcessId>,
    app_event_tx: AppEventSender,

    /// One text area per field.
    fields: [TextArea; FIELD_COUNT],
    /// Render state (scroll offset) for each field.
    field_states: [RefCell<TextAreaState>; FIELD_COUNT],
    /// Index of the currently focused field.
    focused: usize,
    /// Set when the form has been dismissed (submitted or cancelled).
    complete: bool,
    /// Validation error message shown below the form.
    error: Option<String>,
}

impl McpAddForm {
    pub fn new(
        cwd: PathBuf,
        config_layer_stack: ConfigLayerStack,
        process_id: Option<ProcessId>,
        app_event_tx: AppEventSender,
    ) -> Self {
        Self {
            cwd,
            config_layer_stack,
            process_id,
            app_event_tx,
            fields: std::array::from_fn(|_| TextArea::new()),
            field_states: std::array::from_fn(|_| RefCell::new(TextAreaState::default())),
            focused: FIELD_NAME,
            complete: false,
            error: None,
        }
    }

    fn mcp_json_path(&self) -> PathBuf {
        project_mcp_json_path_for_cwd(&self.config_layer_stack, &self.cwd)
    }

    // ── private helpers ──────────────────────────────────────────────────────

    fn advance_field(&mut self) {
        if self.focused < FIELD_COUNT - 1 {
            self.focused += 1;
        } else {
            self.try_submit();
        }
    }

    fn retreat_field(&mut self) {
        if self.focused > 0 {
            self.focused -= 1;
        }
    }

    fn try_submit(&mut self) {
        let name = self.fields[FIELD_NAME].text().trim().to_string();
        let command = self.fields[FIELD_COMMAND].text().trim().to_string();

        if name.is_empty() {
            self.focused = FIELD_NAME;
            self.error = Some("name is required".to_string());
            return;
        }
        if command.is_empty() {
            self.focused = FIELD_COMMAND;
            self.error = Some("command is required".to_string());
            return;
        }

        let is_http = command.starts_with("https://") || command.starts_with("http://");

        let params = if is_http {
            let auth_or_headers = self.fields[FIELD_ENV].text().trim();
            let parsed_http_field = match parse_http_auth_or_headers(auth_or_headers) {
                Ok(parsed) => parsed,
                Err(err) => {
                    self.focused = FIELD_ENV;
                    self.error = Some(err);
                    return;
                }
            };
            McpAddServerParams {
                name,
                command: None,
                args: None,
                env: None,
                url: Some(command),
                bearer_token_env_var: parsed_http_field.bearer_token_env_var,
                http_headers: parsed_http_field.http_headers,
                enabled: None,
                required: None,
            }
        } else {
            // stdio transport
            let args = match parse_stdio_args(self.fields[FIELD_ARGS].text()) {
                Ok(args) => args,
                Err(err) => {
                    self.focused = FIELD_ARGS;
                    self.error = Some(err);
                    return;
                }
            };

            let env: BTreeMap<String, String> = self.fields[FIELD_ENV]
                .text()
                .split(',')
                .filter_map(|pair| {
                    let mut parts = pair.trim().splitn(2, '=');
                    let key = parts.next()?.trim().to_string();
                    let val = parts.next().unwrap_or("").trim().to_string();
                    if key.is_empty() {
                        None
                    } else {
                        Some((key, val))
                    }
                })
                .collect();

            McpAddServerParams {
                name,
                command: Some(command),
                args: (!args.is_empty()).then_some(args),
                env: (!env.is_empty()).then_some(env),
                url: None,
                bearer_token_env_var: None,
                http_headers: None,
                enabled: None,
                required: None,
            }
        };
        let Some(process_id) = self.process_id else {
            self.error = Some("could not reload MCP servers: no active process".to_string());
            return;
        };

        let mcp_json_path = self.mcp_json_path();

        match add_server_to_dot_mcp_json(&mcp_json_path, params) {
            Ok(_) => {
                self.app_event_tx
                    .send(AppEvent::ReloadProjectMcpForProcess(process_id));
                self.complete = true;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("could not write .mcp.json: {e}"));
            }
        }
    }
}

// ── BottomPaneView ────────────────────────────────────────────────────────────

impl BottomPaneView for McpAddForm {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            // Esc cancels.
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.complete = true;
            }

            // Tab → next field.
            KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.advance_field();
            }

            // Shift+Tab → previous field.
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.retreat_field();
            }

            // Enter on final field submits; on others it advances.
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.advance_field();
            }

            // Everything else goes to the active textarea.
            other => {
                self.fields[self.focused].input(other);
                // Clear any previous validation error while the user is typing.
                self.error = None;
            }
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        self.fields[self.focused].insert_str(&pasted);
        true
    }
}

// ── Renderable ────────────────────────────────────────────────────────────────

impl Renderable for McpAddForm {
    fn desired_height(&self, _width: u16) -> u16 {
        // title + (label + input) * FIELD_COUNT + blank + hint [+ error]
        let field_rows: u16 = FIELD_COUNT as u16 * 2;
        let error_row: u16 = if self.error.is_some() { 1 } else { 0 };
        1 + field_rows + 1 + 1 + error_row
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut y = area.y;

        // ── title ─────────────────────────────────────────────────────────────
        if y < area.y + area.height {
            let title_spans: Vec<Span<'static>> = vec![gutter(), "Add MCP server".bold()];
            Paragraph::new(Line::from(title_spans)).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y = y.saturating_add(1);
        }

        // ── fields ────────────────────────────────────────────────────────────
        for idx in 0..FIELD_COUNT {
            if y >= area.y + area.height {
                break;
            }

            let is_focused = idx == self.focused;
            let label = FIELD_LABELS[idx];
            let required = idx == FIELD_NAME || idx == FIELD_COMMAND;
            let required_marker = if required { " *" } else { "" };

            // Label row
            {
                let label_text = format!("  {label}{required_marker}");
                let label_span: Span<'static> = if is_focused {
                    label_text.cyan().bold()
                } else {
                    label_text.dim()
                };
                Paragraph::new(Line::from(vec![label_span])).render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
                y = y.saturating_add(1);
            }

            if y >= area.y + area.height {
                break;
            }

            // Input row
            {
                // Clear the line first so focus highlight is clean.
                Clear.render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );

                // Gutter accent
                let gutter_width: u16 = 2;
                Paragraph::new(Line::from(vec![if is_focused {
                    gutter()
                } else {
                    "  ".into()
                }]))
                .render(
                    Rect {
                        x: area.x,
                        y,
                        width: gutter_width,
                        height: 1,
                    },
                    buf,
                );

                if area.width > gutter_width {
                    let input_rect = Rect {
                        x: area.x + gutter_width,
                        y,
                        width: area.width.saturating_sub(gutter_width),
                        height: 1,
                    };
                    let mut state = self.field_states[idx].borrow_mut();
                    StatefulWidgetRef::render_ref(
                        &(&self.fields[idx]),
                        input_rect,
                        buf,
                        &mut state,
                    );
                    // Show placeholder when empty
                    if self.fields[idx].text().is_empty() {
                        Paragraph::new(Line::from(FIELD_PLACEHOLDERS[idx].to_string().dim()))
                            .render(input_rect, buf);
                    }
                }

                y = y.saturating_add(1);
            }
        }

        // ── error row ─────────────────────────────────────────────────────────
        if let Some(err) = &self.error
            && y < area.y + area.height
        {
            let err_text: Span<'static> = format!("  {err}").red();
            Paragraph::new(Line::from(vec![err_text])).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y = y.saturating_add(1);
        }

        // blank line before hint
        y = y.saturating_add(1);

        // ── hint ──────────────────────────────────────────────────────────────
        if y < area.y + area.height {
            let hint = Line::from(vec![
                "Tab".cyan(),
                "/".into(),
                "Shift+Tab".cyan(),
                " navigate  ".into(),
                "Enter".cyan(),
                " next/submit  ".into(),
                "Esc".cyan(),
                " cancel".into(),
            ]);
            Paragraph::new(hint).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.height == 0 || area.width < 2 {
            return None;
        }

        // title row + (label + input) * focused_field + label row = title(1) + focused*2 + 1
        let input_y = area
            .y
            .saturating_add(1)
            .saturating_add((self.focused as u16) * 2)
            .saturating_add(1);

        if input_y >= area.y + area.height {
            return None;
        }

        let input_rect = Rect {
            x: area.x.saturating_add(2),
            y: input_y,
            width: area.width.saturating_sub(2),
            height: 1,
        };

        let state = *self.field_states[self.focused].borrow();
        self.fields[self.focused].cursor_pos_with_state(input_rect, state)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn gutter() -> Span<'static> {
    "▌ ".cyan()
}

fn parse_stdio_args(input: &str) -> Result<Vec<String>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    shlex::split(trimmed)
        .ok_or_else(|| "could not parse args: check for unmatched quotes".to_string())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedHttpField {
    bearer_token_env_var: Option<String>,
    http_headers: Option<BTreeMap<String, String>>,
}

fn parse_http_auth_or_headers(input: &str) -> Result<ParsedHttpField, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(ParsedHttpField::default());
    }

    if !trimmed.contains('=') {
        if trimmed.contains(',') {
            return Err("enter a single bearer token env var for HTTP auth".to_string());
        }
        return Ok(ParsedHttpField {
            bearer_token_env_var: Some(trimmed.to_string()),
            http_headers: None,
        });
    }

    let http_headers = parse_http_headers(trimmed).ok_or_else(|| {
        if looks_like_mixed_http_auth_input(trimmed) {
            "http auth field must be either KEY=VALUE headers or a single bearer token env var"
                .to_string()
        } else {
            "could not parse HTTP headers: use KEY=VALUE entries separated by commas".to_string()
        }
    })?;

    Ok(ParsedHttpField {
        bearer_token_env_var: None,
        http_headers: (!http_headers.is_empty()).then_some(http_headers),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHeaderEntry {
    key: String,
    value: String,
    score: i32,
}

#[derive(Debug, Clone)]
struct HeaderParseCandidate {
    score: i32,
    count: usize,
    headers: Vec<(String, String)>,
}

fn parse_http_headers(input: &str) -> Option<BTreeMap<String, String>> {
    let chunks: Vec<&str> = input.split(',').collect();
    let mut best: Vec<Option<HeaderParseCandidate>> = vec![None; chunks.len() + 1];
    best[chunks.len()] = Some(HeaderParseCandidate {
        score: 0,
        count: 0,
        headers: Vec::new(),
    });

    for start in (0..chunks.len()).rev() {
        let mut candidate_raw = String::new();
        for end in start + 1..=chunks.len() {
            if end == start + 1 {
                candidate_raw.push_str(chunks[start]);
            } else {
                candidate_raw.push(',');
                candidate_raw.push_str(chunks[end - 1]);
            }

            let Some(parsed_entry) = parse_http_header_entry(&candidate_raw) else {
                continue;
            };
            let Some(rest) = best[end].as_ref() else {
                continue;
            };

            let mut headers = Vec::with_capacity(1 + rest.headers.len());
            headers.push((parsed_entry.key, parsed_entry.value));
            headers.extend(rest.headers.iter().cloned());

            let candidate = HeaderParseCandidate {
                score: parsed_entry.score + rest.score,
                count: 1 + rest.count,
                headers,
            };

            if should_prefer_header_candidate(&candidate, best[start].as_ref()) {
                best[start] = Some(candidate);
            }
        }
    }

    let best = best.into_iter().next().flatten()?;
    Some(best.headers.into_iter().collect())
}

fn parse_http_header_entry(input: &str) -> Option<ParsedHeaderEntry> {
    let (key, value) = input.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    let score = http_header_name_score(key)?;
    Some(ParsedHeaderEntry {
        key: key.to_string(),
        value: value.to_string(),
        score,
    })
}

fn should_prefer_header_candidate(
    candidate: &HeaderParseCandidate,
    existing: Option<&HeaderParseCandidate>,
) -> bool {
    match existing {
        None => true,
        Some(existing) => {
            candidate.score > existing.score
                || (candidate.score == existing.score && candidate.count < existing.count)
        }
    }
}

fn http_header_name_score(name: &str) -> Option<i32> {
    if name.is_empty() || !name.chars().all(is_http_header_name_char) {
        return None;
    }

    let looks_like_typical_header =
        name.len() > 1 || name.chars().any(|ch| ch.is_ascii_uppercase() || ch == '-');

    Some(if looks_like_typical_header { 2 } else { -1 })
}

fn is_http_header_name_char(ch: char) -> bool {
    matches!(
        ch,
        '!' | '#' | '$' | '%' | '&' | '\'' | '*' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~'
    ) || ch.is_ascii_alphanumeric()
}

fn looks_like_mixed_http_auth_input(input: &str) -> bool {
    let entries: Vec<&str> = input
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();

    let has_equals = entries.iter().any(|entry| entry.contains('='));
    let has_bare = entries.iter().any(|entry| !entry.contains('='));
    has_equals && has_bare
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_kern::config_loader::ConfigLayerStack;
    use std::collections::HashMap;
    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;

    fn make_form() -> McpAddForm {
        let (tx, _rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        McpAddForm::new(
            std::env::temp_dir(),
            ConfigLayerStack::default(),
            None,
            sender,
        )
    }

    #[test]
    fn initial_field_is_name() {
        let form = make_form();
        assert_eq!(form.focused, FIELD_NAME);
    }

    #[test]
    fn tab_advances_field() {
        let mut form = make_form();
        form.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(form.focused, FIELD_COMMAND);
    }

    #[test]
    fn shift_tab_goes_back() {
        let mut form = make_form();
        form.focused = FIELD_COMMAND;
        form.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(form.focused, FIELD_NAME);
    }

    #[test]
    fn esc_marks_complete() {
        let mut form = make_form();
        form.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(form.is_complete());
    }

    #[test]
    fn submit_without_name_shows_error() {
        let mut form = make_form();
        // Jump straight to submit path by sitting on final field and pressing Enter
        form.focused = FIELD_ENV;
        form.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(form.error.is_some());
        assert!(!form.is_complete());
    }

    #[test]
    fn args_parsing_uses_shell_style_quoting() {
        let args = parse_stdio_args(r#"--root "/Users/Alice/My Project" --json '{"k":"v"}'"#)
            .expect("parse shell-style args");
        assert_eq!(
            args,
            vec![
                "--root",
                "/Users/Alice/My Project",
                "--json",
                "{\"k\":\"v\"}",
            ]
        );
    }

    #[test]
    fn args_parsing_rejects_unmatched_quotes() {
        let err = parse_stdio_args(r#""unterminated"#).expect_err("should reject bad quoting");
        assert!(err.contains("unmatched quotes"));
    }

    #[test]
    fn env_parsing_splits_key_value() {
        let env: HashMap<String, String> = "FOO=bar,BAZ=qux"
            .split(',')
            .filter_map(|pair| {
                let mut parts = pair.trim().splitn(2, '=');
                let key = parts.next()?.trim().to_string();
                let val = parts.next().unwrap_or("").trim().to_string();
                if key.is_empty() {
                    None
                } else {
                    Some((key, val))
                }
            })
            .collect();
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(env.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn http_field_parses_bearer_env_var_name() {
        assert_eq!(
            parse_http_auth_or_headers("MCP_TOKEN").expect("parse bearer env var"),
            ParsedHttpField {
                bearer_token_env_var: Some("MCP_TOKEN".to_string()),
                http_headers: None,
            }
        );
    }

    #[test]
    fn http_field_parses_headers() {
        assert_eq!(
            parse_http_auth_or_headers("Authorization=Bearer abc,X-Foo=bar")
                .expect("parse headers"),
            ParsedHttpField {
                bearer_token_env_var: None,
                http_headers: Some(BTreeMap::from([
                    ("Authorization".to_string(), "Bearer abc".to_string()),
                    ("X-Foo".to_string(), "bar".to_string()),
                ])),
            }
        );
    }

    #[test]
    fn http_field_preserves_commas_inside_header_values() {
        assert_eq!(
            parse_http_auth_or_headers("Accept=text/html, application/json")
                .expect("parse comma-containing header value"),
            ParsedHttpField {
                bearer_token_env_var: None,
                http_headers: Some(BTreeMap::from([(
                    "Accept".to_string(),
                    "text/html, application/json".to_string(),
                )])),
            }
        );
    }

    #[test]
    fn http_field_preserves_commas_before_embedded_equals_in_header_values() {
        assert_eq!(
            parse_http_auth_or_headers("Cookie=a=1,b=2").expect("parse cookie header value"),
            ParsedHttpField {
                bearer_token_env_var: None,
                http_headers: Some(BTreeMap::from([(
                    "Cookie".to_string(),
                    "a=1,b=2".to_string(),
                )])),
            }
        );
    }

    #[test]
    fn http_field_rejects_mixing_headers_and_env_var_name() {
        let err = parse_http_auth_or_headers("MCP_TOKEN,Authorization=Bearer abc")
            .expect_err("mixed http auth syntax should fail");
        assert!(err.contains("either KEY=VALUE headers or a single bearer token env var"));
    }

    #[test]
    fn submit_without_active_process_does_not_write_mcp_json() {
        let tmp = tempdir().expect("tempdir");
        let (tx, _rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        let mut form = McpAddForm::new(
            tmp.path().to_path_buf(),
            ConfigLayerStack::default(),
            None,
            sender,
        );

        form.fields[FIELD_NAME].insert_str("demo");
        form.fields[FIELD_COMMAND].insert_str("demo-command");
        form.focused = FIELD_ENV;

        form.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            form.error.as_deref(),
            Some("could not reload MCP servers: no active process")
        );
        assert!(!form.is_complete());
        assert!(!form.mcp_json_path().exists());
    }
}
