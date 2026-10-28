use super::*;

#[test]
fn window_size_matches_backend_columns_and_rows() -> io::Result<()> {
    let mut backend = VT100Backend::new(80, 24);
    assert_eq!(backend.size()?, Size::new(80, 24));
    assert_eq!(backend.window_size()?.columns_rows, backend.size()?);
    Ok(())
}

#[test]
fn zero_dimensions_are_clamped_to_one_cell() -> io::Result<()> {
    let backend = VT100Backend::new(0, 0);
    assert_eq!(backend.size()?, Size::new(1, 1));
    Ok(())
}

#[test]
fn shrinking_and_growing_the_screen_preserves_scrollback() -> io::Result<()> {
    let mut backend = VT100Backend::with_scrollback(10, 3, 10);
    backend.write_all(b"one\r\ntwo\r\nthree")?;
    let parser = backend.crossterm_backend.writer_mut();
    for (rows, scrollback, contents) in [(2, 1, "two\nthree"), (3, 0, "one\ntwo\nthree")] {
        parser.set_size(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(10).unwrap());
        assert_eq!(parser.screen().scrollback_len(), scrollback);
        assert_eq!(parser.screen().contents(), contents);
    }
    Ok(())
}
