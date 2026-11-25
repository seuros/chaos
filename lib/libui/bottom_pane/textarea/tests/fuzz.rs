use std::ops::Range;

use rand::prelude::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{StatefulWidgetRef, WidgetRef};
use unicode_width::UnicodeWidthStr;

use super::{TextArea, TextAreaState};

pub(super) fn fixed_corpus() {
    for seed in [1, 42, 20250308] {
        run(seed, 4);
    }
}

pub(crate) fn rotating_stress() {
    let seed = match std::env::var("CHAOS_TEXTAREA_FUZZ_SEED") {
        Ok(seed) => seed
            .parse()
            .expect("CHAOS_TEXTAREA_FUZZ_SEED must be a u64"),
        Err(std::env::VarError::NotPresent) => jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .strftime("%Y%m%d")
            .to_string()
            .parse()
            .expect("UTC date should parse"),
        Err(err) => panic!("invalid CHAOS_TEXTAREA_FUZZ_SEED: {err}"),
    };
    run(seed, 500);
}

fn random_grapheme(rng: &mut StdRng) -> String {
    match rng.random_range(0..100) {
        0..=4 => "\n".into(),
        5..=12 => " ".into(),
        13..=35 => char::from(rng.random_range(b'a'..=b'z')).into(),
        36..=45 => char::from(rng.random_range(b'A'..=b'Z')).into(),
        46..=52 => char::from(rng.random_range(b'0'..=b'9')).into(),
        53..=65 => ["👍", "😊", "🐍", "🚀", "🧪", "🌟"]
            .choose(rng)
            .unwrap()
            .to_string(),
        66..=75 => ["漢", "字", "測", "試", "你", "好", "界", "编", "码"]
            .choose(rng)
            .unwrap()
            .to_string(),
        76..=85 => {
            let base = ["e", "a", "o", "n", "u"].choose(rng).unwrap();
            let mark = ["\u{301}", "\u{308}", "\u{302}", "\u{303}"]
                .choose(rng)
                .unwrap();
            format!("{base}{mark}")
        }
        86..=92 => ["Ω", "β", "Ж", "ю", "ש", "م", "ह"]
            .choose(rng)
            .unwrap()
            .to_string(),
        _ => ["👩‍💻", "👨‍💻", "🏳️‍🌈"].choose(rng).unwrap().to_string(),
    }
}

fn random_text(rng: &mut StdRng, max: usize) -> String {
    (0..rng.random_range(0..=max))
        .map(|_| random_grapheme(rng))
        .collect()
}

fn random_boundary(text: &str, rng: &mut StdRng) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .choose(rng)
        .unwrap()
}

// Only insert_element emits '[...]' sentinels. Apply edits to this plain string
// independently; searching *expected* text tells us exactly which original IDs
// and payloads should still exist, even if the textarea loses both text and range.
struct Model {
    text: String,
    inserted: Vec<(u64, String)>,
}

impl Model {
    fn live_elements(&self) -> impl Iterator<Item = (Range<usize>, u64, &str)> {
        self.inserted.iter().filter_map(|(id, payload)| {
            let start = self.text.find(payload)?;
            Some((start..start + payload.len(), *id, payload.as_str()))
        })
    }

    fn replace(&mut self, range: Range<usize>, replacement: &str) {
        let mut expanded = range.clone();
        for (span, _, _) in self.live_elements() {
            if span.start < range.end && range.start < span.end {
                expanded.start = expanded.start.min(span.start);
                expanded.end = expanded.end.max(span.end);
            }
        }
        self.text.replace_range(expanded, replacement);
    }

    fn check(&self, ta: &TextArea) {
        assert_eq!(ta.text(), self.text);
        assert!(ta.text().is_char_boundary(ta.cursor()));
        let mut expected: Vec<_> = self.live_elements().collect();
        expected.sort_by_key(|(span, _, _)| span.start);
        // Inspect raw ranges, not the snapshot helper (it filters invalid ranges).
        let actual: Vec<_> = ta
            .elements
            .iter()
            .map(|elem| {
                let span = elem.range.clone();
                assert!(ta.cursor() <= span.start || ta.cursor() >= span.end);
                let payload = ta.text().get(span.clone()).expect("invalid element range");
                (span, elem.id, payload)
            })
            .collect();
        assert_eq!(actual, expected);
    }
}

fn edit(op: u8, rng: &mut StdRng, ta: &mut TextArea, model: &mut Model) {
    let c = ta.cursor();
    match op {
        0 => {
            let s = random_text(rng, 5);
            model.text.insert_str(c, &s);
            ta.insert_str(&s);
        }
        1 => {
            let a = random_boundary(&model.text, rng);
            let b = random_boundary(&model.text, rng);
            let range = a.min(b)..a.max(b);
            let s = random_text(rng, 4);
            model.replace(range.clone(), &s);
            ta.replace_range(range, &s);
        }
        2 => {
            let n = rng.random_range(0..=3);
            let start = (0..n).fold(c, |p, _| ta.prev_atomic_boundary(p));
            model.replace(start..c, "");
            ta.delete_backward(n);
        }
        3 => {
            let n = rng.random_range(0..=3);
            let end = (0..n).fold(c, |p, _| ta.next_atomic_boundary(p));
            model.replace(c..end, "");
            ta.delete_forward(n);
        }
        4 => {
            model.replace(ta.beginning_of_previous_word()..c, "");
            ta.delete_backward_word();
        }
        5 => {
            let bol = ta.beginning_of_current_line();
            let start = if c == bol { bol.saturating_sub(1) } else { bol };
            model.replace(start..c, "");
            ta.kill_to_beginning_of_line();
        }
        6 => {
            let eol = ta.end_of_current_line();
            let end = eol + usize::from(c == eol && eol < model.text.len());
            model.replace(c..end, "");
            ta.kill_to_end_of_line();
        }
        7 => ta.move_cursor_left(),
        8 => ta.move_cursor_right(),
        9 => ta.move_cursor_up(),
        10 => ta.move_cursor_down(),
        11 => ta.move_cursor_to_beginning_of_line(true),
        12 => ta.move_cursor_to_end_of_line(true),
        13 => {
            let payload = format!("[[EL#{}]]", model.inserted.len());
            model.text.insert_str(c, &payload);
            let id = ta.insert_element(&payload);
            model.inserted.push((id, payload));
        }
        14..=16 => {
            let Some((span, _, _)) = model.live_elements().choose(rng) else {
                return;
            };
            let p = rng.random_range(span.start + 1..span.end);
            let nearest = if p - span.start <= span.end - p {
                span.start
            } else {
                span.end
            };
            match op {
                14 => {
                    let s = random_grapheme(rng);
                    model.text.insert_str(nearest, &s);
                    ta.insert_str_at(p, &s);
                }
                15 => {
                    let s = random_text(rng, 2);
                    model.text.replace_range(span, &s);
                    ta.replace_range(p..p + 1, &s);
                }
                _ => {
                    ta.set_cursor(p);
                    assert_eq!(ta.cursor(), nearest);
                }
            }
        }
        _ => {
            let pos = if rng.random_bool(0.5) {
                ta.beginning_of_previous_word()
            } else {
                ta.end_of_next_word()
            };
            ta.set_cursor(pos);
        }
    }
}

fn check_render(ta: &TextArea, area: Rect, state: &mut TextAreaState) {
    let full = Rect::new(area.x, area.y, area.width, ta.desired_height(area.width));
    WidgetRef::render_ref(&ta, full, &mut Buffer::empty(full));

    let (col, row) = {
        let lines = ta.wrapped_lines(area.width);
        let row = lines
            .iter()
            .rposition(|line| line.start <= ta.cursor())
            .expect("a nonempty viewport has a wrapped line for the caret");
        // Trailing spaces are preserved; a single wide glyph can exceed the
        // viewport. The caret column is the prefix display width, not a cell bound.
        (
            ta.text()[lines[row].start..ta.cursor()].width() as u16,
            row as u16,
        )
    };
    let min_scroll = (row + 1).saturating_sub(area.height);
    let max_scroll = row.min(full.height.saturating_sub(area.height));
    let scroll = state.scroll.clamp(min_scroll, max_scroll);
    let cursor = |scroll| Some((area.x + col, area.y + row - scroll));
    assert_eq!(ta.cursor_pos(area), cursor(min_scroll));
    assert_eq!(ta.cursor_pos_with_state(area, *state), cursor(scroll));

    StatefulWidgetRef::render_ref(&ta, area, &mut Buffer::empty(area), state);
    assert_eq!(state.scroll, scroll);
}

fn run(seed: u64, cases: usize) {
    eprintln!("textarea fuzz replay: CHAOS_TEXTAREA_FUZZ_SEED={seed}");
    let mut rng = StdRng::seed_from_u64(seed);
    for case in 0..cases {
        let mut model = Model {
            text: random_text(&mut rng, 29),
            inserted: Vec::new(),
        };
        let mut ta = TextArea::new();
        ta.set_text_clearing_elements(&model.text);
        ta.set_cursor(random_boundary(&model.text, &mut rng));
        let mut state = TextAreaState::default();
        let mut area = Rect::new(3, 5, rng.random_range(1..=12), rng.random_range(1..=4));
        for step in 0..60 {
            if rng.random_bool(0.1) {
                area.width = rng.random_range(1..=12);
            }
            if rng.random_bool(0.1) {
                area.height = rng.random_range(1..=4);
            }
            let op = rng.random_range(0..18);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                edit(op, &mut rng, &mut ta, &mut model);
                model.check(&ta);
                check_render(&ta, area, &mut state);
            }));
            if let Err(panic) = result {
                eprintln!("seed={seed} case={case} step={step} op={op} area={area:?}");
                std::panic::resume_unwind(panic);
            }
        }
    }
}
