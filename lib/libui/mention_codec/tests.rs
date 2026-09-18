use super::*;
use pretty_assertions::assert_eq;

fn decode_history_mentions_restores_visible_tokens() {
    let decoded =
        decode_history_mentions("Use [$figma](app://figma-1) and [$figma](/tmp/figma/SKILL.md).");
    assert_eq!(decoded.text, "Use $figma and $figma.");
    assert_eq!(
        decoded.mentions,
        vec![
            LinkedMention {
                mention: "figma".to_string(),
                path: "app://figma-1".to_string(),
            },
            LinkedMention {
                mention: "figma".to_string(),
                path: "/tmp/figma/SKILL.md".to_string(),
            },
        ]
    );
}

pub(crate) fn mention_codec_suite() {
    decode_history_mentions_restores_visible_tokens();
    decode_history_mentions_ignores_at_sigil_links();
    encode_history_mentions_links_bound_mentions_in_order();
}
#[cfg(test)]
fn decode_history_mentions_ignores_at_sigil_links() {
    let decoded = decode_history_mentions("Use [@figma](app://figma-1).");

    assert_eq!(decoded.text, "Use [@figma](app://figma-1).");
    assert_eq!(decoded.mentions, Vec::<LinkedMention>::new());
}

#[cfg(test)]
fn encode_history_mentions_links_bound_mentions_in_order() {
    let text = "$figma then $sample then $figma then $other";
    let encoded = encode_history_mentions(
        text,
        &[
            LinkedMention {
                mention: "figma".to_string(),
                path: "app://figma-app".to_string(),
            },
            LinkedMention {
                mention: "sample".to_string(),
                path: "mcp://sample-server".to_string(),
            },
            LinkedMention {
                mention: "figma".to_string(),
                path: "/tmp/figma/SKILL.md".to_string(),
            },
        ],
    );
    assert_eq!(
        encoded,
        "[$figma](app://figma-app) then [$sample](mcp://sample-server) then [$figma](/tmp/figma/SKILL.md) then $other"
    );
}
