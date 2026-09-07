use std::ops::Range;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::{TextColor, TextStyle};

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
            text_style.foreground = Some(TextColor {
                red: style.foreground.r,
                green: style.foreground.g,
                blue: style.foreground.b,
            });
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

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    THEMES.get_or_init(ThemeSet::load_defaults)
}
