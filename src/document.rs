use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A half-open UTF-8 byte range in the original Markdown source.
pub type SourceSpan = Range<usize>;

/// One parser event together with the exact range reported by pulldown-cmark.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEvent {
    pub event: Event<'static>,
    pub source: SourceSpan,
}

/// Presentation hints derived from Markdown semantics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextStyle {
    pub heading: Option<u8>,
    pub emphasis: bool,
    pub strong: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: bool,
    pub quote: bool,
    pub muted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Segment {
    Text {
        text: String,
        source: SourceSpan,
        style: TextStyle,
        synthetic: bool,
    },
    Break {
        source: usize,
        blank: bool,
    },
}

/// Parsed Markdown content with source identity attached to every fragment.
#[derive(Clone, Debug)]
pub struct Document {
    source: String,
    events: Vec<SourceEvent>,
    pub(crate) segments: Vec<Segment>,
}

impl Document {
    #[must_use]
    pub fn parse(source: String) -> Self {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_MATH);

        let mut builder = Builder::default();
        let mut events = Vec::new();
        for (event, range) in Parser::new_ext(&source, options).into_offset_iter() {
            builder.event(event.clone(), range.clone());
            events.push(SourceEvent {
                event: event.into_static(),
                source: range,
            });
        }
        builder.finish(source, events)
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn source_len(&self) -> usize {
        self.source.len()
    }

    #[must_use]
    pub fn events(&self) -> &[SourceEvent] {
        &self.events
    }
}

#[derive(Debug)]
struct ListState {
    next: Option<u64>,
}

#[derive(Debug, Default)]
struct Builder {
    segments: Vec<Segment>,
    style: TextStyle,
    lists: Vec<ListState>,
    table_cell: usize,
}

impl Builder {
    fn event(&mut self, event: Event<'_>, range: SourceSpan) {
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag, range),
            Event::Text(text) => self.text(text.as_ref(), range, self.style),
            Event::Code(text) => {
                let mut style = self.style;
                style.code = true;
                self.text(text.as_ref(), range, style);
            }
            Event::InlineMath(text) => {
                let mut style = self.style;
                style.code = true;
                self.text(&format!("${text}$"), range, style);
            }
            Event::DisplayMath(text) => {
                self.line_break(range.start, false);
                let mut style = self.style;
                style.code = true;
                self.text(&format!("$${text}$$"), range.clone(), style);
                self.line_break(range.end, true);
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                let mut style = self.style;
                style.muted = true;
                self.text(text.as_ref(), range, style);
            }
            Event::FootnoteReference(label) => {
                self.text(&format!("[^{label}]"), range, self.style);
            }
            Event::SoftBreak => self.text(" ", range, self.style),
            Event::HardBreak => self.line_break(range.end, false),
            Event::Rule => {
                self.line_break(range.start, false);
                self.text("───", range.clone(), self.style);
                self.line_break(range.end, true);
            }
            Event::TaskListMarker(checked) => {
                self.text(
                    if checked { "[x]" } else { "[ ]" },
                    range.clone(),
                    self.style,
                );
                self.synthetic_text(" ", range.end, self.style);
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: SourceSpan) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.line_break(range.start, false);
                self.style.heading = Some(heading_number(level));
            }
            Tag::BlockQuote(_) => {
                self.line_break(range.start, false);
                self.style.quote = true;
                self.synthetic_text("│ ", range.start, self.style);
            }
            Tag::CodeBlock(_) => {
                self.line_break(range.start, false);
                self.style.code = true;
            }
            Tag::HtmlBlock => {
                self.line_break(range.start, false);
                self.style.muted = true;
            }
            Tag::List(start) => self.lists.push(ListState { next: start }),
            Tag::Item => {
                self.line_break(range.start, false);
                let marker = self
                    .lists
                    .last_mut()
                    .and_then(|list| list.next.as_mut())
                    .map(|number| {
                        let marker = format!("{number}. ");
                        *number = number.saturating_add(1);
                        marker
                    })
                    .unwrap_or_else(|| "• ".to_owned());
                self.synthetic_text(&marker, range.start, self.style);
            }
            Tag::Table(_) => {
                self.line_break(range.start, false);
                self.table_cell = 0;
            }
            Tag::TableHead | Tag::TableRow => {
                self.line_break(range.start, false);
                self.table_cell = 0;
            }
            Tag::TableCell => {
                if self.table_cell > 0 {
                    self.synthetic_text(" │ ", range.start, self.style);
                }
                self.table_cell = self.table_cell.saturating_add(1);
            }
            Tag::Emphasis => self.style.emphasis = true,
            Tag::Strong => self.style.strong = true,
            Tag::Strikethrough => self.style.strikethrough = true,
            Tag::Link { .. } => self.style.link = true,
            Tag::Image { .. } => {
                self.synthetic_text("[image: ", range.start, self.style);
            }
            Tag::FootnoteDefinition(label) => {
                self.line_break(range.start, false);
                self.synthetic_text(&format!("[^{label}]: "), range.start, self.style);
            }
            Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd, range: SourceSpan) {
        match tag {
            TagEnd::Paragraph => self.line_break(range.end, true),
            TagEnd::Heading(_) => {
                self.style.heading = None;
                self.line_break(range.end, true);
            }
            TagEnd::BlockQuote(_) => {
                self.style.quote = false;
                self.line_break(range.end, true);
            }
            TagEnd::CodeBlock => {
                self.style.code = false;
                self.line_break(range.end, true);
            }
            TagEnd::HtmlBlock => {
                self.style.muted = false;
                self.line_break(range.end, true);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.line_break(range.end, true);
            }
            TagEnd::Item => self.line_break(range.end, false),
            TagEnd::Table => self.line_break(range.end, true),
            TagEnd::TableHead | TagEnd::TableRow => self.line_break(range.end, false),
            TagEnd::TableCell => {}
            TagEnd::Emphasis => self.style.emphasis = false,
            TagEnd::Strong => self.style.strong = false,
            TagEnd::Strikethrough => self.style.strikethrough = false,
            TagEnd::Link => self.style.link = false,
            TagEnd::Image => {
                self.synthetic_text("]", range.end, self.style);
            }
            TagEnd::FootnoteDefinition => self.line_break(range.end, true),
            TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {}
        }
    }

    fn text(&mut self, text: &str, source: SourceSpan, style: TextStyle) {
        if !text.is_empty() {
            self.segments.push(Segment::Text {
                text: text.to_owned(),
                source,
                style,
                synthetic: false,
            });
        }
    }

    fn synthetic_text(&mut self, text: &str, source: usize, style: TextStyle) {
        if !text.is_empty() {
            self.segments.push(Segment::Text {
                text: text.to_owned(),
                source: source..source,
                style,
                synthetic: true,
            });
        }
    }

    fn line_break(&mut self, source: usize, blank: bool) {
        let duplicate = self.segments.last().is_some_and(|segment| {
            matches!(segment, Segment::Break { blank: existing, .. } if *existing || !blank)
        });
        if !duplicate {
            self.segments.push(Segment::Break { source, blank });
        }
    }

    fn finish(mut self, source: String, events: Vec<SourceEvent>) -> Document {
        if self.segments.is_empty() {
            self.segments.push(Segment::Break {
                source: 0,
                blank: false,
            });
        }
        Document {
            source,
            events,
            segments: self.segments,
        }
    }
}

const fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_extensions_keep_source_ranges() {
        let source =
            "# Title\n\n- [x] **bold** and ~~old~~ with $x$\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let document = Document::parse(source.to_owned());

        let text_segments: Vec<_> = document
            .segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text { text, source, .. } => Some((text, source)),
                Segment::Break { .. } => None,
            })
            .collect();

        assert!(text_segments.iter().any(|(text, _)| text == &"[x]"));
        assert!(text_segments.iter().any(|(text, _)| text == &"$x$"));
        assert!(text_segments.iter().any(|(text, _)| text == &" │ "));
        assert!(
            text_segments.iter().all(|(_, range)| {
                range.start <= range.end && range.end <= document.source_len()
            })
        );
        assert!(!document.events().is_empty());
        assert!(document.events().iter().all(|event| {
            event.source.start <= event.source.end && event.source.end <= document.source_len()
        }));
    }
}
