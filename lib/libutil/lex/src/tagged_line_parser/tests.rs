use super::TagSpec;
use super::TaggedLineParser;
use super::TaggedLineSegment;
use pretty_assertions::assert_eq;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Block,
}

fn parser() -> TaggedLineParser<Tag> {
    TaggedLineParser::new(vec![TagSpec {
        open: "<tag>",
        close: "</tag>",
        tag: Tag::Block,
    }])
}

#[test]
fn buffers_prefix_until_tag_is_decided() {
    let mut parser = parser();
    let mut segments = parser.parse("<t");
    segments.extend(parser.parse("ag>\nline\n</tag>\n"));
    segments.extend(parser.finish());

    assert_eq!(
        segments,
        vec![
            TaggedLineSegment::TagStart(Tag::Block),
            TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
            TaggedLineSegment::TagEnd(Tag::Block),
        ]
    );
}

#[test]
fn rejects_tag_lines_with_extra_text() {
    let mut parser = parser();
    let mut segments = parser.parse("<tag> extra\n");
    segments.extend(parser.finish());

    assert_eq!(
        segments,
        vec![TaggedLineSegment::Normal("<tag> extra\n".to_string())]
    );
}
