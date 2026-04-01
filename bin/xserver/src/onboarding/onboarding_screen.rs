use chaos_kern::config::Config;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::widgets::Clear;
use ratatui::widgets::WidgetRef;

use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::onboarding::welcome::WelcomeWidget;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;

enum Step {
    Welcome(WelcomeWidget),
    TrustDirectory(TrustDirectoryWidget),
}

pub(crate) trait KeyboardHandler {
    fn handle_key_event(&mut self, key_event: KeyEvent);
    fn handle_paste(&mut self, _pasted: String) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Hidden,
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    steps: Vec<Step>,
    is_done: bool,
    should_exit: bool,
}

pub(crate) struct OnboardingScreenArgs {
    pub show_trust_screen: bool,
    pub config: Config,
}

pub(crate) struct OnboardingResult {
    pub directory_trust_decision: Option<TrustDirectorySelection>,
    pub should_exit: bool,
}

impl OnboardingScreen {
    pub(crate) fn new(_tui: &mut Tui, args: OnboardingScreenArgs) -> Self {
        let OnboardingScreenArgs {
            show_trust_screen,
            config,
        } = args;
        let cwd = config.cwd.clone();
        let chaos_home = config.chaos_home.clone();
        let mut steps: Vec<Step> = Vec::new();
        steps.push(Step::Welcome(WelcomeWidget::new(true)));
        if show_trust_screen {
            steps.push(Step::TrustDirectory(TrustDirectoryWidget {
                cwd,
                chaos_home,
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: None,
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            }))
        }
        Self {
            steps,
            is_done: false,
            should_exit: false,
        }
    }

    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        let mut out: Vec<&mut Step> = Vec::new();
        for step in self.steps.iter_mut() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    fn current_steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = Vec::new();
        for step in self.steps.iter() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
            || !self
                .steps
                .iter()
                .any(|step| matches!(step.get_step_state(), StepState::InProgress))
    }

    pub fn directory_trust_decision(&self) -> Option<TrustDirectorySelection> {
        self.steps
            .iter()
            .find_map(|step| {
                if let Step::TrustDirectory(TrustDirectoryWidget { selection, .. }) = step {
                    Some(*selection)
                } else {
                    None
                }
            })
            .flatten()
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }
}

impl KeyboardHandler for OnboardingScreen {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        let should_quit = match key_event {
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => true,
            KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            } => true,
            _ => false,
        };
        if should_quit {
            self.is_done = true;
        } else {
            if let Some(active_step) = self.current_steps_mut().into_iter().last() {
                active_step.handle_key_event(key_event);
            }
            if self.steps.iter().any(|step| {
                if let Step::TrustDirectory(widget) = step {
                    widget.should_quit()
                } else {
                    false
                }
            }) {
                self.should_exit = true;
                self.is_done = true;
            }
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }

        if let Some(active_step) = self.current_steps_mut().into_iter().last() {
            active_step.handle_paste(pasted);
        }
    }
}

impl WidgetRef for &OnboardingScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);
        let width = area.width;

        fn used_rows(tmp: &Buffer, width: u16, height: u16) -> u16 {
            if width == 0 || height == 0 {
                return 0;
            }
            let mut last_non_empty: Option<u16> = None;
            for yy in 0..height {
                let mut any = false;
                for xx in 0..width {
                    let cell = &tmp[(xx, yy)];
                    let has_symbol = !cell.symbol().trim().is_empty();
                    let has_style = cell.fg != Color::Reset
                        || cell.bg != Color::Reset
                        || !cell.modifier.is_empty();
                    if has_symbol || has_style {
                        any = true;
                        break;
                    }
                }
                if any {
                    last_non_empty = Some(yy);
                }
            }
            last_non_empty.map(|v| v + 2).unwrap_or(0)
        }

        let mut i = 0usize;
        let current_steps = self.current_steps();

        while i < current_steps.len() && y < bottom {
            let step = &current_steps[i];
            let max_h = bottom.saturating_sub(y);
            if max_h == 0 || width == 0 {
                break;
            }
            let scratch_area = Rect::new(0, 0, width, max_h);
            let mut scratch = Buffer::empty(scratch_area);
            step.render_ref(scratch_area, &mut scratch);
            let h = used_rows(&scratch, width, max_h).min(max_h);
            if h > 0 {
                let target = Rect {
                    x: area.x,
                    y,
                    width,
                    height: h,
                };
                Clear.render(target, buf);
                step.render_ref(target, buf);
                y = y.saturating_add(h);
            }
            i += 1;
        }
    }
}

impl KeyboardHandler for Step {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self {
            Step::Welcome(widget) => widget.handle_key_event(key_event),
            Step::TrustDirectory(widget) => widget.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Step::Welcome(_) => {}
            Step::TrustDirectory(widget) => widget.handle_paste(pasted),
        }
    }
}

impl StepStateProvider for Step {
    fn get_step_state(&self) -> StepState {
        match self {
            Step::Welcome(w) => w.get_step_state(),
            Step::TrustDirectory(w) => w.get_step_state(),
        }
    }
}

impl WidgetRef for Step {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Step::Welcome(widget) => widget.render_ref(area, buf),
            Step::TrustDirectory(widget) => widget.render_ref(area, buf),
        }
    }
}

pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    use tokio_stream::StreamExt;

    let mut onboarding_screen = OnboardingScreen::new(tui, args);

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&onboarding_screen, frame.area());
    })?;

    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);

    while !onboarding_screen.is_done() {
        if let Some(event) = tui_events.next().await {
            match event {
                TuiEvent::Key(key_event) => {
                    onboarding_screen.handle_key_event(key_event);
                }
                TuiEvent::Paste(text) => {
                    onboarding_screen.handle_paste(text);
                }
                TuiEvent::Draw => {
                    let _ = tui.draw(u16::MAX, |frame| {
                        frame.render_widget_ref(&onboarding_screen, frame.area());
                    });
                }
            }
        }
    }
    Ok(OnboardingResult {
        directory_trust_decision: onboarding_screen.directory_trust_decision(),
        should_exit: onboarding_screen.should_exit(),
    })
}
