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

/// A local Markdown image before path resolution or decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageElement {
    pub destination: String,
    pub title: String,
    pub alt: String,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// A level-one or level-two heading that can be projected as text or raster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingElement {
    pub level: u8,
    pub text: String,
    pub source: SourceSpan,
    pub style: TextStyle,
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
    Image {
        index: usize,
    },
    Heading {
        index: usize,
    },
}

/// Parsed Markdown content with source identity attached to every fragment.
#[derive(Clone, Debug)]
pub struct Document {
    source: String,
    events: Vec<SourceEvent>,
    pub(crate) segments: Vec<Segment>,
    images: Vec<ImageElement>,
    headings: Vec<HeadingElement>,
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

    #[must_use]
    pub fn images(&self) -> &[ImageElement] {
        &self.images
    }

    #[must_use]
    pub fn headings(&self) -> &[HeadingElement] {
        &self.headings
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
    image: Option<ImageBuilder>,
    images: Vec<ImageElement>,
    heading: Option<HeadingBuilder>,
    headings: Vec<HeadingElement>,
}

#[derive(Debug)]
struct ImageBuilder {
    destination: String,
    title: String,
    alt: String,
    source: SourceSpan,
    style: TextStyle,
}

#[derive(Debug)]
struct HeadingBuilder {
    level: u8,
    text: String,
    source: SourceSpan,
    style: TextStyle,
}

impl Builder {
    fn event(&mut self, event: Event<'_>, range: SourceSpan) {
        if self.heading.is_some() {
            match event {
                Event::End(TagEnd::Heading(_)) => self.finish_heading(range),
                Event::Text(text) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str(text.as_ref());
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::Code(text) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push('`');
                        heading.text.push_str(text.as_ref());
                        heading.text.push('`');
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::InlineMath(text) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push('$');
                        heading.text.push_str(text.as_ref());
                        heading.text.push('$');
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::DisplayMath(text) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str("$$");
                        heading.text.push_str(text.as_ref());
                        heading.text.push_str("$$");
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::FootnoteReference(label) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str("[^");
                        heading.text.push_str(label.as_ref());
                        heading.text.push(']');
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::Html(text) | Event::InlineHtml(text) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str(text.as_ref());
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push(' ');
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::Start(Tag::Image { .. }) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str("[image: ");
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::End(TagEnd::Image) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push(']');
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::Rule => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str("───");
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                Event::TaskListMarker(checked) => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.text.push_str(if checked { "[x] " } else { "[ ] " });
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
                _ => {
                    if let Some(heading) = self.heading.as_mut() {
                        heading.source.end = heading.source.end.max(range.end);
                    }
                }
            }
            return;
        }
        if self.image.is_some() {
            match event {
                Event::End(TagEnd::Image) => self.finish_image(range),
                Event::Text(text) | Event::Code(text) => {
                    if let Some(image) = self.image.as_mut() {
                        image.alt.push_str(text.as_ref());
                        image.source.end = image.source.end.max(range.end);
                    }
                }
                _ => {
                    if let Some(image) = self.image.as_mut() {
                        image.source.end = image.source.end.max(range.end);
                    }
                }
            }
            return;
        }

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
                let level = heading_number(level);
                if level <= 2 {
                    let mut style = self.style;
                    style.heading = Some(level);
                    self.heading = Some(HeadingBuilder {
                        level,
                        text: String::new(),
                        source: range,
                        style,
                    });
                } else {
                    self.style.heading = Some(level);
                }
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
            Tag::Image {
                dest_url, title, ..
            } => {
                self.image = Some(ImageBuilder {
                    destination: dest_url.into_string(),
                    title: title.into_string(),
                    alt: String::new(),
                    source: range,
                    style: self.style,
                });
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
            TagEnd::Image => {}
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

    fn finish_image(&mut self, range: SourceSpan) {
        let Some(mut image) = self.image.take() else {
            return;
        };
        image.source.end = image.source.end.max(range.end);
        let index = self.images.len();
        self.images.push(ImageElement {
            destination: image.destination,
            title: image.title,
            alt: image.alt,
            source: image.source,
            style: image.style,
        });
        self.segments.push(Segment::Image { index });
    }

    fn finish_heading(&mut self, range: SourceSpan) {
        let Some(mut heading) = self.heading.take() else {
            return;
        };
        heading.source.end = heading.source.end.max(range.end);
        let index = self.headings.len();
        self.headings.push(HeadingElement {
            level: heading.level,
            text: heading.text,
            source: heading.source,
            style: heading.style,
        });
        self.segments.push(Segment::Heading { index });
        self.line_break(range.end, true);
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
            images: self.images,
            headings: self.headings,
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
                Segment::Break { .. } | Segment::Image { .. } | Segment::Heading { .. } => None,
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

    #[test]
    fn images_retain_destination_alt_text_and_source_range() {
        let source = "before\n\n![diagram alt](assets/diagram.png \"Diagram\")\n\nafter";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.images().len(), 1);
        let image = &document.images()[0];
        assert_eq!(image.destination, "assets/diagram.png");
        assert_eq!(image.alt, "diagram alt");
        assert_eq!(image.title, "Diagram");
        assert_eq!(image.style, TextStyle::default());
        assert_eq!(
            &source[image.source.clone()],
            "![diagram alt](assets/diagram.png \"Diagram\")"
        );
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Image { index: 0 }))
        );
    }

    #[test]
    fn large_headings_are_semantic_elements_with_source_ranges() {
        let source = "# **Large** 漆\n\n## Second level\n\n### Terminal text\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.headings().len(), 2);
        assert_eq!(document.headings()[0].level, 1);
        assert_eq!(document.headings()[0].text, "Large 漆");
        assert_eq!(
            &source[document.headings()[0].source.clone()],
            "# **Large** 漆\n"
        );
        assert_eq!(document.headings()[1].level, 2);
        assert_eq!(document.headings()[1].text, "Second level");
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Heading { index: 0 }))
        );
        assert!(document.segments.iter().any(|segment| matches!(
            segment,
            Segment::Text { text, style, .. }
                if text == "Terminal text" && style.heading == Some(3)
        )));
    }

    #[test]
    fn large_heading_projection_preserves_inline_semantics() {
        let source = "# Energy $E=mc^2$, `code`, and ![diagram](image.png)\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.headings().len(), 1);
        assert_eq!(
            document.headings()[0].text,
            "Energy $E=mc^2$, `code`, and [image: diagram]"
        );
        assert_eq!(document.headings()[0].source, 0..source.len());
    }
}
