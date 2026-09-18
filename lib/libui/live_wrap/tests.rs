use super::*;
use pretty_assertions::assert_eq;

fn rows_do_not_exceed_width_ascii() {
    let mut rb = RowBuilder::new(10);
    rb.push_fragment("hello whirl this is a test");
    let rows = rb.rows().to_vec();
    assert_eq!(
        rows,
        vec![
            Row {
                text: "hello whir".to_string(),
                explicit_break: false
            },
            Row {
                text: "l this is ".to_string(),
                explicit_break: false
            }
        ]
    );
}

pub(crate) fn live_wrap_suite() {
    rows_do_not_exceed_width_ascii();
    rows_do_not_exceed_width_emoji_cjk();
    fragmentation_invariance_long_token();
    newline_splits_rows();
    rewrap_on_width_change();
}
#[cfg(test)]
fn rows_do_not_exceed_width_emoji_cjk() {
    // 😀 is width 2; 你/好 are width 2.
    let mut rb = RowBuilder::new(6);
    rb.push_fragment("😀😀 你好");
    let rows = rb.rows().to_vec();
    // At width 6, we expect the first row to fit exactly two emojis and a space
    // (2 + 2 + 1 = 5) plus one more column for the first CJK char (2 would overflow),
    // so only the two emojis and the space fit; the rest remains buffered.
    assert_eq!(
        rows,
        vec![Row {
            text: "😀😀 ".to_string(),
            explicit_break: false
        }]
    );
}

#[cfg(test)]
fn fragmentation_invariance_long_token() {
    let s = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"; // 26 chars
    let mut rb_all = RowBuilder::new(7);
    rb_all.push_fragment(s);
    let all_rows = rb_all.rows().to_vec();

    let mut rb_chunks = RowBuilder::new(7);
    for i in (0..s.len()).step_by(3) {
        let end = (i + 3).min(s.len());
        rb_chunks.push_fragment(&s[i..end]);
    }
    let chunk_rows = rb_chunks.rows().to_vec();

    assert_eq!(all_rows, chunk_rows);
}

#[cfg(test)]
fn newline_splits_rows() {
    let mut rb = RowBuilder::new(10);
    rb.push_fragment("hello\nworld");
    let rows = rb.display_rows();
    assert!(rows.iter().any(|r| r.explicit_break));
    assert_eq!(rows[0].text, "hello");
    // Second row should begin with 'world'
    assert!(rows.iter().any(|r| r.text.starts_with("world")));
}

#[cfg(test)]
fn rewrap_on_width_change() {
    let mut rb = RowBuilder::new(10);
    rb.push_fragment("abcdefghijK");
    assert!(!rb.rows().is_empty());
    rb.set_width(5);
    for r in rb.rows() {
        assert!(r.width() <= 5);
    }
}
