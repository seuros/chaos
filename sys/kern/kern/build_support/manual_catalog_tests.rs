use super::*;

const PAGE: &str = "+++\ntitle = 'Example'\nsummary = 'An example manual.'\n+++\n\n# Example\n";

#[test]
fn extracts_metadata_and_body_with_lf_or_crlf() {
    for source in [PAGE.to_owned(), PAGE.replace('\n', "\r\n")] {
        let page = parse_page("example.7", &source).unwrap();
        assert_eq!(page.id, "example.7");
        assert_eq!(page.title, "Example");
        assert_eq!(page.summary, "An example manual.");
        assert!(page.body.starts_with("# Example"));
        assert!(!page.body.contains("+++"));
        assert!(!page.body.contains("summary ="));
    }
}

#[test]
fn rejects_missing_malformed_or_incomplete_frontmatter() {
    for source in [
        "# Missing header\n",
        "+++\ntitle = 'Unterminated'\n",
        "+++\ntitle = 42\nsummary = 'Summary'\n+++\nBody",
        "+++\ntitle = 'Title'\n+++\nBody",
        "+++\ntitle = ''\nsummary = 'Summary'\n+++\nBody",
        "+++\ntitle = 'Title'\nsummary = '  '\n+++\nBody",
        "+++\ntitle = \"Line\\nbreak\"\nsummary = 'Summary'\n+++\nBody",
        "+++\ntitle = 'Title'\nsummary = 'Summary'\nid = 'other.7'\n+++\nBody",
        "+++\ntitle = 'Title'\nsummary = 'Summary'\n+++\n",
        "+++\ninvalid TOML\n+++\nBody",
    ] {
        assert!(parse_page("example.7", source).is_err(), "{source}");
    }
    assert!(parse_page("not a uri.7", PAGE).is_err());
}

#[test]
fn discovers_additions_and_removals_in_stable_order_without_a_registry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    std::fs::write(path.join("README.md"), "# Not a manual").unwrap();
    std::fs::write(path.join("notes.md"), "# Not a manual").unwrap();
    std::fs::create_dir(path.join("nested")).unwrap();
    std::fs::write(path.join("nested/hidden.7.md"), PAGE).unwrap();
    std::fs::write(path.join("zebra.8.md"), PAGE).unwrap();
    std::fs::write(path.join("alpha.1.md"), PAGE).unwrap();
    let pages = read_catalog(path).unwrap();
    assert_eq!(
        pages
            .iter()
            .map(|page| page.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha.1", "zebra.8"]
    );

    std::fs::write(path.join("new.7.md"), PAGE).unwrap();
    let added = read_catalog(path).unwrap();
    assert_eq!(added.len(), 3);
    assert_eq!(added[1].id, "new.7");
    std::fs::remove_file(path.join("new.7.md")).unwrap();
    assert_eq!(read_catalog(path).unwrap(), pages);

    std::fs::write(path.join("broken.7.md"), "# No frontmatter").unwrap();
    let error = read_catalog(path).unwrap_err();
    assert!(error.contains("broken.7.md"), "{error}");
    assert!(error.contains("frontmatter"), "{error}");
}

#[test]
fn generated_catalog_contains_escaped_body_not_metadata_or_file_paths() {
    let source = format!("{PAGE}\n\"quoted\" \\ path ```rust``` λ\n");
    let page = parse_page("example.7", &source).unwrap();
    let expected_literal = format!("{:?}", page.body);
    let generated = render_catalog(&[page]);
    assert!(generated.contains(&format!("source: {expected_literal}")));
    assert!(!generated.contains("summary ="));
    assert!(!generated.contains("+++"));
    assert!(!generated.contains("include_str!"));
    assert_eq!(render_catalog(&[]), "&[\n]\n");
}
