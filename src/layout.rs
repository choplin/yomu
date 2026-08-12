use std::ops::Range;

use unicode_width::UnicodeWidthChar;

use crate::document::{Document, Segment};
use crate::{SourceSpan, TextStyle};

/// A displayed text fragment that retains its original source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutFragment {
    pub text: String,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// One terminal row and the source range represented by that row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutRow {
    pub fragments: Vec<LayoutFragment>,
    pub source: SourceSpan,
}

/// Width-dependent terminal layout derived from a source-ranged document.
#[derive(Clone, Debug)]
pub struct Layout {
    width: usize,
    rows: Vec<LayoutRow>,
}

impl Layout {
    #[must_use]
    pub fn new(document: &Document, width: u16) -> Self {
        let width = usize::from(width.max(1));
        let mut builder = Builder::new(width);
        for segment in &document.segments {
            match segment {
                Segment::Text {
                    text,
                    source,
                    style,
                    synthetic,
                } => builder.text(text, source.clone(), *style, *synthetic),
                Segment::Break { source, blank } => builder.line_break(*source, *blank),
            }
        }
        builder.finish(document.source_len())
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn rows(&self) -> &[LayoutRow] {
        &self.rows
    }

    #[must_use]
    pub fn row_for_source(&self, offset: usize) -> usize {
        self.rows
            .iter()
            .position(|row| {
                (row.source.start <= offset && offset < row.source.end)
                    || row.source.start >= offset
            })
            .unwrap_or_else(|| self.rows.len().saturating_sub(1))
    }
}

#[derive(Debug)]
struct Builder {
    width: usize,
    cell_width: usize,
    rows: Vec<LayoutRow>,
    current: Vec<LayoutFragment>,
    anchor: usize,
}

impl Builder {
    fn new(width: usize) -> Self {
        Self {
            width,
            cell_width: 0,
            rows: Vec::new(),
            current: Vec::new(),
            anchor: 0,
        }
    }

    fn text(&mut self, text: &str, source: SourceSpan, style: TextStyle, synthetic: bool) {
        if synthetic {
            self.synthetic_text(text, source, style);
            return;
        }

        let mut piece_start = 0;
        for (byte_start, character) in text.char_indices() {
            if character == '\n' {
                self.text_piece(text, source.clone(), style, piece_start..byte_start);
                let mapped = map_source(&source, text.len(), byte_start);
                self.push_row(mapped.start);
                piece_start = byte_start.saturating_add(character.len_utf8());
                continue;
            }
            if character.is_whitespace() {
                let piece_end = byte_start.saturating_add(character.len_utf8());
                self.text_piece(text, source.clone(), style, piece_start..piece_end);
                piece_start = piece_end;
            }
        }
        self.text_piece(text, source, style, piece_start..text.len());
    }

    fn synthetic_text(&mut self, text: &str, source: SourceSpan, style: TextStyle) {
        let available = self.width.saturating_sub(self.cell_width);
        let available = if self.current_has_source() {
            available
        } else {
            available.saturating_sub(1)
        };
        let mut used = 0usize;
        for (byte_start, character) in text.char_indices() {
            let character_width = character.width().unwrap_or(0);
            if used.saturating_add(character_width) > available {
                break;
            }
            let byte_end = byte_start.saturating_add(character.len_utf8());
            let mapped = map_source_range(&source, text.len(), byte_start..byte_end);
            self.push_character(character, mapped, style);
            self.cell_width = self.cell_width.saturating_add(character_width);
            used = used.saturating_add(character_width);
        }
    }

    fn text_piece(
        &mut self,
        text: &str,
        source: SourceSpan,
        style: TextStyle,
        bytes: Range<usize>,
    ) {
        let Some(piece) = text.get(bytes.clone()) else {
            return;
        };
        if piece.is_empty() {
            return;
        }
        let piece_width = piece
            .chars()
            .map(|character| character.width().unwrap_or(0))
            .sum::<usize>();
        if self.cell_width > 0
            && self.current_has_source()
            && !piece.trim().is_empty()
            && piece_width <= self.width
            && self.cell_width.saturating_add(piece_width) > self.width
        {
            let mapped = map_source(&source, text.len(), bytes.start);
            self.push_row(mapped.start);
        }

        for (relative_start, character) in piece.char_indices() {
            let byte_start = bytes.start.saturating_add(relative_start);
            let character_width = character.width().unwrap_or(0);
            if self.cell_width > 0 && self.cell_width.saturating_add(character_width) > self.width {
                let mapped = map_source(&source, text.len(), byte_start);
                self.push_row(mapped.start);
            }

            let byte_end = byte_start.saturating_add(character.len_utf8());
            let mapped = map_source_range(&source, text.len(), byte_start..byte_end);
            self.push_character(character, mapped, style);
            self.cell_width = self.cell_width.saturating_add(character_width);
        }
    }

    fn push_character(&mut self, character: char, source: SourceSpan, style: TextStyle) {
        if let Some(last) = self.current.last_mut()
            && last.style == style
            && last.source.end == source.start
        {
            last.text.push(character);
            last.source.end = source.end;
            return;
        }

        self.current.push(LayoutFragment {
            text: character.to_string(),
            source,
            style,
        });
    }

    fn current_has_source(&self) -> bool {
        self.current
            .iter()
            .any(|fragment| !fragment.source.is_empty())
    }

    fn line_break(&mut self, source: usize, blank: bool) {
        if !self.current.is_empty() {
            self.push_row(source);
        }
        if blank
            && self
                .rows
                .last()
                .is_some_and(|row| !row.fragments.is_empty())
        {
            self.anchor = source;
            self.push_row(source);
        }
    }

    fn push_row(&mut self, fallback: usize) {
        let source = row_source(&self.current).unwrap_or(fallback..fallback);
        self.rows.push(LayoutRow {
            fragments: std::mem::take(&mut self.current),
            source,
        });
        self.cell_width = 0;
        self.anchor = fallback;
    }

    fn finish(mut self, document_len: usize) -> Layout {
        if !self.current.is_empty() {
            self.push_row(document_len);
        }
        if self.rows.is_empty() {
            self.rows.push(LayoutRow {
                fragments: Vec::new(),
                source: self.anchor..self.anchor,
            });
        }
        Layout {
            width: self.width,
            rows: self.rows,
        }
    }
}

fn row_source(fragments: &[LayoutFragment]) -> Option<SourceSpan> {
    let start = fragments
        .iter()
        .filter(|fragment| !fragment.source.is_empty())
        .map(|fragment| fragment.source.start)
        .min()?;
    let end = fragments
        .iter()
        .filter(|fragment| !fragment.source.is_empty())
        .map(|fragment| fragment.source.end)
        .max()?;
    Some(start..end)
}

fn map_source(source: &SourceSpan, text_len: usize, byte: usize) -> SourceSpan {
    map_source_range(source, text_len, byte..byte)
}

fn map_source_range(source: &SourceSpan, text_len: usize, bytes: Range<usize>) -> SourceSpan {
    if source.is_empty() || text_len == 0 {
        return source.clone();
    }

    let source_len = source.end.saturating_sub(source.start);
    let map = |byte: usize| {
        source
            .start
            .saturating_add(byte.saturating_mul(source_len) / text_len)
            .min(source.end)
    };
    map(bytes.start)..map(bytes.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_preserves_ordered_source_anchors() {
        let document = Document::parse("A paragraph with enough words to wrap twice.".to_owned());
        let layout = Layout::new(&document, 12);

        assert!(layout.rows().len() >= 3);
        assert!(
            layout
                .rows()
                .windows(2)
                .all(|rows| { rows[0].source.start <= rows[1].source.start })
        );
        assert!(layout.rows().iter().all(|row| {
            row.fragments.iter().all(|fragment| {
                fragment.source.start <= fragment.source.end
                    && fragment.source.end <= document.source_len()
            })
        }));
    }

    #[test]
    fn source_anchor_survives_relayout() {
        let document = Document::parse(
            "First logical paragraph has enough content to wrap over several rows.\n\nSecond paragraph."
                .to_owned(),
        );
        let wide = Layout::new(&document, 30);
        let anchor = wide.rows()[1].source.start;
        let narrow = Layout::new(&document, 12);
        let restored = &narrow.rows()[narrow.row_for_source(anchor)];

        assert!(anchor > 0);
        assert!(restored.source.end >= anchor || restored.source.start >= anchor);
        assert!(restored.source.start > 0);
    }
}
