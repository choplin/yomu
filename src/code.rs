use std::ops::Range;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::{TextStyle, document::SyntaxRole};

const BASE16_OCEAN_DARK: [[u8; 3]; 16] = [
    [0x2b, 0x30, 0x3b],
    [0x34, 0x3d, 0x46],
    [0x4f, 0x5b, 0x66],
    [0x65, 0x73, 0x7e],
    [0xa7, 0xad, 0xba],
    [0xc0, 0xc5, 0xce],
    [0xdf, 0xe1, 0xe8],
    [0xef, 0xf1, 0xf5],
    [0xbf, 0x61, 0x6a],
    [0xd0, 0x87, 0x70],
    [0xeb, 0xcb, 0x8b],
    [0xa3, 0xbe, 0x8c],
    [0x96, 0xb5, 0xb4],
    [0x8f, 0xa1, 0xb3],
    [0xb4, 0x8e, 0xad],
    [0xab, 0x79, 0x67],
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HighlightedSpan {
    pub range: Range<usize>,
    pub style: TextStyle,
}

/// Highlight a complete code block, retaining newlines in the returned spans.
pub(crate) fn highlight(
    code: &str,
    language: Option<&str>,
    base_style: TextStyle,
) -> Option<Vec<HighlightedSpan>> {
    let language = language?;
    let syntaxes = syntaxes();
    let syntax = syntaxes.find_syntax_by_token(language)?;
    let theme = themes().themes.get("base16-ocean.dark")?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut spans = Vec::new();
    let mut offset = 0usize;

    for line in LinesWithEndings::from(code) {
        for (style, text) in highlighter.highlight_line(line, syntaxes).ok()? {
            let mut text_style = base_style;
            text_style.syntax_role =
                syntax_role([style.foreground.r, style.foreground.g, style.foreground.b]);
            let end = offset.saturating_add(text.len());
            spans.push(HighlightedSpan {
                range: offset..end,
                style: text_style,
            });
            offset = end;
        }
    }

    Some(spans)
}

fn syntax_role(color: [u8; 3]) -> Option<SyntaxRole> {
    let base16_index = BASE16_OCEAN_DARK
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| {
            candidate
                .iter()
                .zip(color)
                .map(|(left, right)| i32::from(*left).abs_diff(i32::from(right)))
                .sum::<u32>()
        })
        .map_or(5, |(index, _)| index);
    match base16_index {
        3 => Some(SyntaxRole::Comment),
        8 => Some(SyntaxRole::Variable),
        9 => Some(SyntaxRole::Number),
        10 => Some(SyntaxRole::TypeName),
        11 => Some(SyntaxRole::String),
        12 => Some(SyntaxRole::Support),
        13 => Some(SyntaxRole::Function),
        14 => Some(SyntaxRole::Keyword),
        15 => Some(SyntaxRole::Deprecated),
        _ => None,
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    THEMES.get_or_init(ThemeSet::load_defaults)
}
