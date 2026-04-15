//! Public-API tests for `chaos-lex` — streaming parsers that turn
//! ragged token soup into stable chunks.
//!
//! The shared contract: a parser feeds on whatever bytes arrive, buffers
//! whatever could still be a tag, and emits only the parts that are
//! provably safe to render or store. These tests exercise every parser
//! across chunk boundaries that deliberately land inside tag openers,
//! close markers, and UTF-8 multi-byte sequences — the entropy hotspots
//! where streaming parsers drift into silent corruption.

use chaos_lex::AssistantTextStreamParser;
use chaos_lex::CitationStreamParser;
use chaos_lex::InlineHiddenTagParser;
use chaos_lex::InlineTagSpec;
use chaos_lex::ProposedPlanParser;
use chaos_lex::ProposedPlanSegment;
use chaos_lex::StreamTextChunk;
use chaos_lex::StreamTextParser;
use chaos_lex::Utf8StreamParser;
use chaos_lex::Utf8StreamParserError;
use chaos_lex::collect_chunks;
use chaos_lex::extract_proposed_plan_text;
use chaos_lex::strip_citations;
use chaos_lex::strip_proposed_plan_blocks;
use pretty_assertions::assert_eq;

// ── assistant_text (composed citation + plan parser) ───────────────────

#[test]
fn assistant_text_stream_splits_citations_across_chunks_and_streams_plan_segments() {
    // Citation straddles the boundary between two delivered chunks.
    let mut parser = AssistantTextStreamParser::new(false);
    let seeded = parser.push_str("hello <oai-mem-citation>doc");
    let parsed = parser.push_str("1</oai-mem-citation> world");
    let tail = parser.finish();
    assert_eq!(seeded.visible_text, "hello ");
    assert_eq!(seeded.citations, Vec::<String>::new());
    assert_eq!(parsed.visible_text, " world");
    assert_eq!(parsed.citations, vec!["doc1".to_string()]);
    assert!(tail.visible_text.is_empty());
    assert!(tail.citations.is_empty());

    // With proposed-plan parsing enabled, a nested citation inside a
    // plan block should be reported as a citation AND the plan block
    // should emit start/delta/end segments in order.
    let mut parser = AssistantTextStreamParser::new(true);
    let seeded = parser.push_str("Intro\n<prop");
    let parsed = parser.push_str("osed_plan>\n- step <oai-mem-citation>doc</oai-mem-citation>\n");
    let tail = parser.push_str("</proposed_plan>\nOutro");
    let finish = parser.finish();

    assert_eq!(seeded.visible_text, "Intro\n");
    assert_eq!(
        seeded.plan_segments,
        vec![ProposedPlanSegment::Normal("Intro\n".to_string())]
    );
    assert!(parsed.visible_text.is_empty());
    assert_eq!(parsed.citations, vec!["doc".to_string()]);
    assert_eq!(
        parsed.plan_segments,
        vec![
            ProposedPlanSegment::ProposedPlanStart,
            ProposedPlanSegment::ProposedPlanDelta("- step \n".to_string()),
        ]
    );
    assert_eq!(tail.visible_text, "Outro");
    assert_eq!(
        tail.plan_segments,
        vec![
            ProposedPlanSegment::ProposedPlanEnd,
            ProposedPlanSegment::Normal("Outro".to_string()),
        ]
    );
    assert!(finish.is_empty());
}

// ── citation parser ─────────────────────────────────────────────────────

#[test]
fn citation_parser_streams_partial_tags_and_handles_finish_semantics() {
    // Open tag split mid-marker across chunks.
    let mut parser = CitationStreamParser::new();
    let out = collect_chunks(
        &mut parser,
        &[
            "Hello <oai-mem-",
            "citation>source A</oai-mem-",
            "citation> world",
        ],
    );
    assert_eq!(out.visible_text, "Hello  world");
    assert_eq!(out.extracted, vec!["source A".to_string()]);

    // A partial open-tag prefix at the end of a chunk must be buffered,
    // not emitted — otherwise half a tag leaks into the visible stream.
    let mut parser = CitationStreamParser::new();
    let first = parser.push_str("abc <oai-mem-");
    assert_eq!(first.visible_text, "abc ");
    assert!(first.extracted.is_empty());
    let second = parser.push_str("citation>x</oai-mem-citation>z");
    let tail = parser.finish();
    assert_eq!(second.visible_text, "z");
    assert_eq!(second.extracted, vec!["x".to_string()]);
    assert!(tail.is_empty());

    // Unterminated open at EOF is closed implicitly so callers never
    // lose the citation body.
    let mut parser = CitationStreamParser::new();
    let out = collect_chunks(&mut parser, &["x<oai-mem-citation>source"]);
    assert_eq!(out.visible_text, "x");
    assert_eq!(out.extracted, vec!["source".to_string()]);

    // If nothing past the partial open matches, the bytes fall through
    // to visible output on finish — no silent drops.
    let mut parser = CitationStreamParser::new();
    let out = collect_chunks(&mut parser, &["hello <oai-mem-"]);
    assert_eq!(out.visible_text, "hello <oai-mem-");
    assert!(out.extracted.is_empty());

    // Batch helper collects every citation and strips them in order.
    let (visible, citations) = strip_citations(
        "a<oai-mem-citation>one</oai-mem-citation>b<oai-mem-citation>two</oai-mem-citation>c",
    );
    assert_eq!(visible, "abc");
    assert_eq!(citations, vec!["one".to_string(), "two".to_string()]);

    // Batch helper also auto-closes a dangling open tag at EOF.
    let (visible, citations) = strip_citations("x<oai-mem-citation>y");
    assert_eq!(visible, "x");
    assert_eq!(citations, vec!["y".to_string()]);

    // Nested tags are intentionally NOT supported: the inner open is
    // treated as content, and the inner close re-surfaces as literal
    // text after the outer close.
    let (visible, citations) = strip_citations(
        "a<oai-mem-citation>x<oai-mem-citation>y</oai-mem-citation>z</oai-mem-citation>b",
    );
    assert_eq!(visible, "az</oai-mem-citation>b");
    assert_eq!(citations, vec!["x<oai-mem-citation>y".to_string()]);
}

// ── generic inline hidden-tag parser ────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    A,
    B,
}

#[test]
fn inline_hidden_tag_parser_handles_multi_spec_unicode_and_longest_match() {
    // Multiple tag specs run concurrently; payloads land on the right
    // tag discriminant.
    let mut parser = InlineHiddenTagParser::new(vec![
        InlineTagSpec {
            tag: Tag::A,
            open: "<a>",
            close: "</a>",
        },
        InlineTagSpec {
            tag: Tag::B,
            open: "<b>",
            close: "</b>",
        },
    ]);
    let out = collect_chunks(&mut parser, &["1<a>x</a>2<b>y</b>3"]);
    assert_eq!(out.visible_text, "123");
    assert_eq!(out.extracted.len(), 2);
    assert_eq!(out.extracted[0].tag, Tag::A);
    assert_eq!(out.extracted[0].content, "x");
    assert_eq!(out.extracted[1].tag, Tag::B);
    assert_eq!(out.extracted[1].content, "y");

    // Non-ASCII delimiters: tag bytes span a chunk boundary mid-UTF-8.
    let mut parser = InlineHiddenTagParser::new(vec![InlineTagSpec {
        tag: Tag::A,
        open: "<é>",
        close: "</é>",
    }]);
    let out = collect_chunks(&mut parser, &["a<", "é>中</", "é>b"]);
    assert_eq!(out.visible_text, "ab");
    assert_eq!(out.extracted.len(), 1);
    assert_eq!(out.extracted[0].tag, Tag::A);
    assert_eq!(out.extracted[0].content, "中");

    // Longest-opener-wins at the same offset: `<ab>` outranks `<a>` when
    // both start at the same byte.
    let mut parser = InlineHiddenTagParser::new(vec![
        InlineTagSpec {
            tag: Tag::A,
            open: "<a>",
            close: "</a>",
        },
        InlineTagSpec {
            tag: Tag::B,
            open: "<ab>",
            close: "</ab>",
        },
    ]);
    let out = collect_chunks(&mut parser, &["x<ab>y</ab>z"]);
    assert_eq!(out.visible_text, "xz");
    assert_eq!(out.extracted.len(), 1);
    assert_eq!(out.extracted[0].tag, Tag::B);
    assert_eq!(out.extracted[0].content, "y");
}

#[test]
#[should_panic(expected = "non-empty open delimiters")]
fn inline_hidden_tag_parser_rejects_empty_open_delimiter() {
    let _ = InlineHiddenTagParser::new(vec![InlineTagSpec {
        tag: Tag::A,
        open: "",
        close: "</a>",
    }]);
}

#[test]
#[should_panic(expected = "non-empty close delimiters")]
fn inline_hidden_tag_parser_rejects_empty_close_delimiter() {
    let _ = InlineHiddenTagParser::new(vec![InlineTagSpec {
        tag: Tag::A,
        open: "<a>",
        close: "",
    }]);
}

// ── proposed plan parser ────────────────────────────────────────────────

#[test]
fn proposed_plan_parser_streams_segments_and_matches_batch_helpers() {
    // Plan block straddles three chunks; visible text is the non-block
    // content in order, and extracted segments include start/delta/end
    // around the tag payload.
    let mut parser = ProposedPlanParser::new();
    let out = collect_chunks(
        &mut parser,
        &[
            "Intro text\n<prop",
            "osed_plan>\n- step 1\n",
            "</proposed_plan>\nOutro",
        ],
    );
    assert_eq!(out.visible_text, "Intro text\nOutro");
    assert_eq!(
        out.extracted,
        vec![
            ProposedPlanSegment::Normal("Intro text\n".to_string()),
            ProposedPlanSegment::ProposedPlanStart,
            ProposedPlanSegment::ProposedPlanDelta("- step 1\n".to_string()),
            ProposedPlanSegment::ProposedPlanEnd,
            ProposedPlanSegment::Normal("Outro".to_string()),
        ]
    );

    // A line that starts with whitespace in front of the tag opener is
    // NOT a plan block — it must round-trip as normal content.
    let mut parser = ProposedPlanParser::new();
    let out = collect_chunks(&mut parser, &["  <proposed_plan> extra\n"]);
    assert_eq!(out.visible_text, "  <proposed_plan> extra\n");
    assert_eq!(
        out.extracted,
        vec![ProposedPlanSegment::Normal(
            "  <proposed_plan> extra\n".to_string()
        )]
    );

    // Unterminated plan block gets an implicit end on finish so callers
    // don't have to reason about half-open streams.
    let mut parser = ProposedPlanParser::new();
    let out = collect_chunks(&mut parser, &["<proposed_plan>\n- step 1\n"]);
    assert!(out.visible_text.is_empty());
    assert_eq!(
        out.extracted,
        vec![
            ProposedPlanSegment::ProposedPlanStart,
            ProposedPlanSegment::ProposedPlanDelta("- step 1\n".to_string()),
            ProposedPlanSegment::ProposedPlanEnd,
        ]
    );

    // The batch helpers agree with the streaming parser.
    let text = "before\n<proposed_plan>\n- step\n</proposed_plan>\nafter";
    assert_eq!(strip_proposed_plan_blocks(text), "before\nafter");
    assert_eq!(
        extract_proposed_plan_text(text),
        Some("- step\n".to_string())
    );
}

// ── UTF-8 stream parser ─────────────────────────────────────────────────

fn collect_bytes(
    parser: &mut Utf8StreamParser<CitationStreamParser>,
    chunks: &[&[u8]],
) -> Result<StreamTextChunk<String>, Utf8StreamParserError> {
    let mut all = StreamTextChunk::default();
    for chunk in chunks {
        let next = parser.push_bytes(chunk)?;
        all.visible_text.push_str(&next.visible_text);
        all.extracted.extend(next.extracted);
    }
    let tail = parser.finish()?;
    all.visible_text.push_str(&tail.visible_text);
    all.extracted.extend(tail.extracted);
    Ok(all)
}

#[test]
fn utf8_stream_parser_handles_multi_byte_splits_and_reports_invalid_sequences() {
    // A 2-byte code point and a 3-byte code point both get split across
    // chunk boundaries. The parser must re-assemble both before feeding
    // the inner parser, so "é" and "中" land intact.
    let chunks: [&[u8]; 3] = [
        b"A\xC3",
        b"\xA9<oai-mem-citation>\xE4",
        b"\xB8\xAD</oai-mem-citation>Z",
    ];
    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let out = collect_bytes(&mut parser, &chunks).expect("valid utf-8 stream");
    assert_eq!(out.visible_text, "AéZ");
    assert_eq!(out.extracted, vec!["中".to_string()]);

    // A lone leading byte may be buffered (no output) until the next
    // chunk clarifies whether it's valid. An invalid continuation byte
    // yields a specific InvalidUtf8 error, after which the parser can
    // still recover on a fresh chunk.
    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let first = parser
        .push_bytes(&[0xC3])
        .expect("leading byte may be buffered");
    assert!(first.is_empty());
    let err = parser
        .push_bytes(&[0x28])
        .expect_err("invalid continuation must error");
    assert_eq!(
        err,
        Utf8StreamParserError::InvalidUtf8 {
            valid_up_to: 0,
            error_len: 1,
        }
    );
    let second = parser
        .push_bytes(&[0xA9, b'x'])
        .expect("recover after rollback");
    let tail = parser.finish().expect("finish after recovery");
    assert_eq!(second.visible_text, "éx");
    assert!(second.extracted.is_empty());
    assert!(tail.is_empty());

    // Invalid byte mid-chunk: the entire chunk is rolled back but the
    // reported `valid_up_to` still points at the good prefix length.
    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let err = parser
        .push_bytes(b"ok\xFF")
        .expect_err("bad byte must error");
    assert_eq!(
        err,
        Utf8StreamParserError::InvalidUtf8 {
            valid_up_to: 2,
            error_len: 1,
        }
    );
    let next = parser.push_bytes(b"!").expect("recover after rollback");
    assert_eq!(next.visible_text, "!");
    assert!(next.extracted.is_empty());

    // A partial code point buffered at EOF is reported as incomplete —
    // `finish`, `into_inner`, and `into_inner_lossy` each pick a lane:
    // finish errors, into_inner errors, into_inner_lossy drops.
    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let out = parser
        .push_bytes(&[0xE2, 0x82])
        .expect("partial code point buffered");
    assert!(out.is_empty());
    let err = parser
        .finish()
        .expect_err("unfinished code point must error");
    assert_eq!(err, Utf8StreamParserError::IncompleteUtf8AtEof);

    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let out = parser
        .push_bytes(&[0xC3])
        .expect("partial code point buffered");
    assert!(out.is_empty());
    let err = parser
        .into_inner()
        .expect_err("buffered partial must be rejected");
    assert_eq!(err, Utf8StreamParserError::IncompleteUtf8AtEof);

    let mut parser = Utf8StreamParser::new(CitationStreamParser::new());
    let out = parser
        .push_bytes(&[0xC3])
        .expect("partial code point buffered");
    assert!(out.is_empty());
    let mut inner = parser.into_inner_lossy();
    let tail = inner.finish();
    assert!(tail.is_empty());
}
