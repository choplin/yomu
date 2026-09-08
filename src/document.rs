use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::code::{self, HighlightedSpan};

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

/// An ordinary Markdown link whose destination survives into the rendered view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkElement {
    pub destination: String,
    pub title: String,
    pub label: String,
    pub source: SourceSpan,
    pub label_source: SourceSpan,
    pub destination_source: SourceSpan,
    pub style: TextStyle,
    /// Whether the link rendered any label content, including a nested image.
    pub has_label: bool,
}

impl LinkElement {
    /// Whether the label already carries the destination, so showing both would duplicate it.
    #[must_use]
    pub fn is_collapsed(&self) -> bool {
        let label = self.label.trim();
        label == self.destination
            || self
                .destination
                .strip_prefix("mailto:")
                .is_some_and(|address| address == label)
    }

    /// The subdued destination shown after the label, or `None` when it would duplicate it.
    #[must_use]
    pub fn destination_projection(&self) -> Option<String> {
        if self.destination.is_empty() || self.is_collapsed() {
            return None;
        }
        Some(if self.has_label {
            format!(" <{}>", self.destination)
        } else {
            format!("<{}>", self.destination)
        })
    }
}

/// A level-one or level-two heading that can be projected as text or raster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingElement {
    pub level: u8,
    pub text: String,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// Whether a Markdown math expression participates in a line or owns a block.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MathForm {
    Inline,
    Display,
}

/// A source-ranged TeX math expression before conversion and rasterization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MathElement {
    pub expression: String,
    pub form: MathForm,
    pub source: SourceSpan,
    pub style: TextStyle,
}

impl MathElement {
    #[must_use]
    pub fn text_projection(&self) -> String {
        match self.form {
            MathForm::Inline => format!("${}$", self.expression),
            MathForm::Display => format!("$${}$$", self.expression),
        }
    }
}

/// Horizontal alignment declared for one GFM table column.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TableAlignment {
    #[default]
    Default,
    Left,
    Center,
    Right,
}

/// One source-ranged cell in a parsed GFM table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCell {
    pub text: String,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// One source-ranged row in a parsed GFM table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRow {
    pub header: bool,
    pub cells: Vec<TableCell>,
    pub source: SourceSpan,
}

/// A parsed GFM table that can be projected as terminal text or a raster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableElement {
    pub alignments: Vec<TableAlignment>,
    pub rows: Vec<TableRow>,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// A fenced or indented Markdown code block with its source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeBlockElement {
    /// The complete fenced info string, or `None` for an indented block.
    pub info: Option<String>,
    /// The first whitespace-delimited word of the fenced info string.
    pub language: Option<String>,
    pub text: String,
    pub source: SourceSpan,
    /// The enclosing source range; per-fragment mapping retains any gaps inside it.
    pub content_source: SourceSpan,
    pub style: TextStyle,
    pub(crate) parts: Vec<CodeBlockPart>,
    pub(crate) fragments: Vec<CodeBlockFragment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodeBlockPart {
    pub text: Range<usize>,
    pub source: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodeBlockFragment {
    pub text: Range<usize>,
    pub source: SourceSpan,
    pub style: TextStyle,
}

/// An RGB terminal text color derived from document presentation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
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
    pub foreground: Option<TextColor>,
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
    Rule {
        source: SourceSpan,
        style: TextStyle,
    },
    Image {
        index: usize,
    },
    Heading {
        index: usize,
    },
    Table {
        index: usize,
    },
    Math {
        index: usize,
    },
    Link {
        index: usize,
    },
    CodeBlock {
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
    tables: Vec<TableElement>,
    math: Vec<MathElement>,
    links: Vec<LinkElement>,
    code_blocks: Vec<CodeBlockElement>,
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

    #[must_use]
    pub fn tables(&self) -> &[TableElement] {
        &self.tables
    }

    #[must_use]
    pub fn math(&self) -> &[MathElement] {
        &self.math
    }

    #[must_use]
    pub fn links(&self) -> &[LinkElement] {
        &self.links
    }

    #[must_use]
    pub fn code_blocks(&self) -> &[CodeBlockElement] {
        &self.code_blocks
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
    pending_list_marker: Option<usize>,
    table: Option<TableBuilder>,
    tables: Vec<TableElement>,
    image: Option<ImageBuilder>,
    images: Vec<ImageElement>,
    heading: Option<HeadingBuilder>,
    headings: Vec<HeadingElement>,
    math: Vec<MathElement>,
    link: Option<LinkBuilder>,
    links: Vec<LinkElement>,
    code_block: Option<CodeBlockBuilder>,
    code_blocks: Vec<CodeBlockElement>,
}

#[derive(Debug)]
struct CodeBlockBuilder {
    info: Option<String>,
    language: Option<String>,
    text: String,
    source: SourceSpan,
    parts: Vec<CodeBlockPart>,
    style: TextStyle,
}

#[derive(Debug)]
struct LinkBuilder {
    destination: String,
    title: String,
    label: String,
    source: SourceSpan,
    label_source: Option<SourceSpan>,
    style: TextStyle,
    segment_start: usize,
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

#[derive(Debug)]
struct TableBuilder {
    alignments: Vec<TableAlignment>,
    rows: Vec<TableRow>,
    row: Option<TableRowBuilder>,
    cell: Option<TableCellBuilder>,
    source: SourceSpan,
    style: TextStyle,
}

#[derive(Debug)]
struct TableRowBuilder {
    header: bool,
    cells: Vec<TableCell>,
    source: SourceSpan,
}

#[derive(Debug)]
struct TableCellBuilder {
    text: String,
    source: SourceSpan,
    style: TextStyle,
}

impl Builder {
    fn event(&mut self, event: Event<'_>, range: SourceSpan) {
        if matches!(&event, Event::TaskListMarker(_)) {
            self.discard_pending_list_marker();
        }
        if self.code_block.is_some() {
            match event {
                Event::End(TagEnd::CodeBlock) => self.finish_code_block(range),
                Event::Text(text) | Event::Code(text) => {
                    if let Some(code_block) = self.code_block.as_mut() {
                        let text_start = code_block.text.len();
                        code_block.text.push_str(text.as_ref());
                        let text_end = code_block.text.len();
                        code_block.parts.push(CodeBlockPart {
                            text: text_start..text_end,
                            source: range.clone(),
                        });
                        code_block.source.end = code_block.source.end.max(range.end);
                    }
                }
                _ => {
                    if let Some(code_block) = self.code_block.as_mut() {
                        code_block.source.end = code_block.source.end.max(range.end);
                    }
                }
            }
            return;
        }
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
                        heading.text.push_str(task_list_marker(checked));
                        heading.text.push_str("  ");
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
        if self.table.is_some() {
            self.table_event(event, range);
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
                self.math(text.as_ref(), MathForm::Inline, range);
            }
            Event::DisplayMath(text) => {
                self.line_break(range.start, false);
                self.math(text.as_ref(), MathForm::Display, range.clone());
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
                self.rule(range.clone());
                self.line_break(range.end, true);
            }
            Event::TaskListMarker(checked) => {
                self.text(task_list_marker(checked), range.clone(), self.style);
                self.synthetic_text("  ", range.end, self.style);
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
            Tag::CodeBlock(kind) => {
                self.line_break(range.start, false);
                let info = match kind {
                    CodeBlockKind::Indented => None,
                    CodeBlockKind::Fenced(info) => Some(info.into_string()),
                };
                let language = info
                    .as_deref()
                    .and_then(|info| info.split_whitespace().next())
                    .filter(|language| !language.is_empty())
                    .map(str::to_owned);
                let mut style = self.style;
                style.code = true;
                self.code_block = Some(CodeBlockBuilder {
                    info,
                    language,
                    text: String::new(),
                    source: range,
                    parts: Vec::new(),
                    style,
                });
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
                self.pending_list_marker = Some(self.segments.len());
                self.synthetic_text(&marker, range.start, self.style);
            }
            Tag::Table(alignments) => {
                self.line_break(range.start, false);
                self.table = Some(TableBuilder {
                    alignments: alignments.into_iter().map(TableAlignment::from).collect(),
                    rows: Vec::new(),
                    row: None,
                    cell: None,
                    source: range,
                    style: self.style,
                });
            }
            Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.style.emphasis = true,
            Tag::Strong => self.style.strong = true,
            Tag::Strikethrough => self.style.strikethrough = true,
            Tag::Link {
                dest_url, title, ..
            } => {
                self.style.link = true;
                self.link = Some(LinkBuilder {
                    destination: dest_url.into_string(),
                    title: title.into_string(),
                    label: String::new(),
                    source: range,
                    label_source: None,
                    style: self.style,
                    segment_start: self.segments.len(),
                });
            }
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
                // Code block endings are consumed while the block builder is active.
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
            TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::Emphasis => self.style.emphasis = false,
            TagEnd::Strong => self.style.strong = false,
            TagEnd::Strikethrough => self.style.strikethrough = false,
            TagEnd::Link => {
                self.style.link = false;
                self.finish_link(range);
            }
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
        if let Some(link) = self.link.as_mut() {
            link.label.push_str(text);
            link.label_source = Some(match link.label_source.take() {
                Some(existing) => existing.start.min(source.start)..existing.end.max(source.end),
                None => source.clone(),
            });
        }
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

    fn discard_pending_list_marker(&mut self) {
        let Some(index) = self.pending_list_marker.take() else {
            return;
        };
        if index + 1 == self.segments.len()
            && matches!(
                self.segments.get(index),
                Some(Segment::Text {
                    synthetic: true,
                    ..
                })
            )
        {
            self.segments.pop();
        }
    }

    /// Record a thematic break as a width-independent element resolved at layout time.
    fn rule(&mut self, source: SourceSpan) {
        self.segments.push(Segment::Rule {
            source,
            style: self.style,
        });
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

    fn math(&mut self, expression: &str, form: MathForm, source: SourceSpan) {
        let index = self.math.len();
        self.math.push(MathElement {
            expression: expression.to_owned(),
            form,
            source,
            style: self.style,
        });
        self.segments.push(Segment::Math { index });
    }

    fn finish_link(&mut self, range: SourceSpan) {
        let Some(mut link) = self.link.take() else {
            return;
        };
        link.source.end = link.source.end.max(range.end);
        let label_source = link
            .label_source
            .unwrap_or(link.source.start..link.source.start);
        let has_label = self.segments.len() > link.segment_start;
        let index = self.links.len();
        self.links.push(LinkElement {
            destination: link.destination,
            title: link.title,
            label: link.label,
            source: link.source,
            label_source,
            // Resolved against the document source once parsing completes.
            destination_source: 0..0,
            style: link.style,
            has_label,
        });
        self.segments.push(Segment::Link { index });
    }

    fn finish_code_block(&mut self, range: SourceSpan) {
        let Some(mut code_block) = self.code_block.take() else {
            return;
        };
        code_block.source.end = code_block.source.end.max(range.end);
        let content_source = code_block.parts.first().map_or(
            code_block.source.start..code_block.source.start,
            |first| {
                let end = code_block
                    .parts
                    .last()
                    .map_or(first.source.end, |last| last.source.end);
                first.source.start..end
            },
        );
        let highlighted = code::highlight(
            &code_block.text,
            code_block.language.as_deref(),
            code_block.style,
        );
        let fragments =
            code_block_fragments(&code_block.parts, highlighted.as_deref(), code_block.style);
        let index = self.code_blocks.len();
        self.code_blocks.push(CodeBlockElement {
            info: code_block.info,
            language: code_block.language,
            text: code_block.text,
            source: code_block.source,
            content_source,
            style: code_block.style,
            parts: code_block.parts,
            fragments,
        });
        self.segments.push(Segment::CodeBlock { index });
        self.line_break(range.end, true);
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

    fn table_event(&mut self, event: Event<'_>, range: SourceSpan) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        table.source.end = table.source.end.max(range.end);
        match event {
            Event::Start(Tag::TableHead) => table.start_row(true, range),
            Event::Start(Tag::TableRow) => table.start_row(false, range),
            Event::Start(Tag::TableCell) => table.start_cell(range),
            Event::End(TagEnd::TableCell) => table.finish_cell(range),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => table.finish_row(range),
            Event::End(TagEnd::Table) => self.finish_table(range),
            Event::Text(text) => table.push_text(text.as_ref(), range),
            Event::Code(text) => table.push_wrapped("`", text.as_ref(), "`", range),
            Event::InlineMath(text) => table.push_wrapped("$", text.as_ref(), "$", range),
            Event::DisplayMath(text) => table.push_wrapped("$$", text.as_ref(), "$$", range),
            Event::SoftBreak | Event::HardBreak => table.push_text(" ", range),
            Event::FootnoteReference(label) => {
                table.push_wrapped("[^", label.as_ref(), "]", range);
            }
            Event::Html(text) | Event::InlineHtml(text) => table.push_text(text.as_ref(), range),
            Event::TaskListMarker(checked) => {
                table.push_text(task_list_marker(checked), range.clone());
                table.push_text("  ", range);
            }
            Event::Start(Tag::Image { .. }) => table.push_text("[image: ", range),
            Event::End(TagEnd::Image) => table.push_text("]", range),
            Event::Rule => table.push_text("---", range),
            Event::Start(_) | Event::End(_) => {}
        }
    }

    fn finish_table(&mut self, range: SourceSpan) {
        let Some(mut table) = self.table.take() else {
            return;
        };
        table.finish_cell(range.clone());
        table.finish_row(range.clone());
        table.source.end = table.source.end.max(range.end);
        let index = self.tables.len();
        self.tables.push(TableElement {
            alignments: table.alignments,
            rows: table.rows,
            source: table.source,
            style: table.style,
        });
        self.segments.push(Segment::Table { index });
        self.line_break(range.end, true);
    }

    fn finish(mut self, source: String, events: Vec<SourceEvent>) -> Document {
        for link in &mut self.links {
            link.destination_source = destination_span(&source, link);
        }
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
            tables: self.tables,
            math: self.math,
            links: self.links,
            code_blocks: self.code_blocks,
        }
    }
}

fn code_block_fragments(
    parts: &[CodeBlockPart],
    highlighted: Option<&[HighlightedSpan]>,
    fallback_style: TextStyle,
) -> Vec<CodeBlockFragment> {
    let Some(highlighted) = highlighted else {
        return parts
            .iter()
            .map(|part| CodeBlockFragment {
                text: part.text.clone(),
                source: part.source.clone(),
                style: fallback_style,
            })
            .collect();
    };

    let mut fragments = Vec::new();
    let mut part_index = 0usize;
    for span in highlighted {
        while let Some(part) = parts.get(part_index) {
            if part.text.end <= span.range.start {
                part_index = part_index.saturating_add(1);
                continue;
            }
            if part.text.start >= span.range.end {
                break;
            }

            let start = span.range.start.max(part.text.start);
            let end = span.range.end.min(part.text.end);
            if start < end {
                fragments.push(CodeBlockFragment {
                    text: start..end,
                    source: map_code_part_source(part, start..end),
                    style: span.style,
                });
            }
            if part.text.end <= span.range.end {
                part_index = part_index.saturating_add(1);
            } else {
                break;
            }
        }
    }
    fragments
}

fn map_code_part_source(part: &CodeBlockPart, text: Range<usize>) -> SourceSpan {
    if part.source.is_empty() || part.text.is_empty() {
        return part.source.clone();
    }
    let text_len = part.text.end.saturating_sub(part.text.start);
    let source_len = part.source.end.saturating_sub(part.source.start);
    let map = |offset: usize| {
        part.source
            .start
            .saturating_add(offset.saturating_mul(source_len) / text_len)
            .min(part.source.end)
    };
    map(text.start.saturating_sub(part.text.start))..map(text.end.saturating_sub(part.text.start))
}

/// Locate the destination inside the link's own source text, preferring the text after its label.
fn destination_span(source: &str, link: &LinkElement) -> SourceSpan {
    let tail_start = link
        .label_source
        .end
        .clamp(link.source.start, link.source.end);
    let fallback = tail_start..link.source.end;
    if link.destination.is_empty() {
        return fallback;
    }
    let Some(text) = source.get(link.source.clone()) else {
        return fallback;
    };
    let tail_offset = tail_start.saturating_sub(link.source.start);
    let found = text
        .get(tail_offset..)
        .and_then(|tail| tail.find(link.destination.as_str()))
        .map(|offset| tail_offset.saturating_add(offset))
        .or_else(|| text.find(link.destination.as_str()));
    found.map_or(fallback, |offset| {
        let start = link.source.start.saturating_add(offset);
        start..start.saturating_add(link.destination.len())
    })
}

impl TableBuilder {
    fn start_row(&mut self, header: bool, range: SourceSpan) {
        self.finish_cell(range.clone());
        self.finish_row(range.clone());
        self.row = Some(TableRowBuilder {
            header,
            cells: Vec::new(),
            source: range,
        });
    }

    fn start_cell(&mut self, range: SourceSpan) {
        self.finish_cell(range.clone());
        if self.row.is_none() {
            self.start_row(false, range.clone());
        }
        self.cell = Some(TableCellBuilder {
            text: String::new(),
            source: range,
            style: self.style,
        });
    }

    fn push_text(&mut self, text: &str, range: SourceSpan) {
        if self.cell.is_none() {
            self.start_cell(range.clone());
        }
        if let Some(cell) = self.cell.as_mut() {
            cell.text.push_str(text);
            cell.source.end = cell.source.end.max(range.end);
        }
    }

    fn push_wrapped(&mut self, prefix: &str, text: &str, suffix: &str, range: SourceSpan) {
        self.push_text(&format!("{prefix}{text}{suffix}"), range);
    }

    fn finish_cell(&mut self, range: SourceSpan) {
        let Some(mut cell) = self.cell.take() else {
            return;
        };
        cell.source.end = cell.source.end.max(range.end);
        if let Some(row) = self.row.as_mut() {
            row.source.end = row.source.end.max(cell.source.end);
            row.cells.push(TableCell {
                text: cell.text,
                source: cell.source,
                style: cell.style,
            });
        }
    }

    fn finish_row(&mut self, range: SourceSpan) {
        self.finish_cell(range.clone());
        let Some(mut row) = self.row.take() else {
            return;
        };
        row.source.end = row.source.end.max(range.end);
        if !row.cells.is_empty() {
            self.rows.push(TableRow {
                header: row.header,
                cells: row.cells,
                source: row.source,
            });
        }
    }
}

impl From<Alignment> for TableAlignment {
    fn from(value: Alignment) -> Self {
        match value {
            Alignment::None => Self::Default,
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
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

const fn task_list_marker(checked: bool) -> &'static str {
    if checked { "☑" } else { "☐" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_extensions_keep_source_ranges() {
        let source = "# Title\n\n- [x] **bold** and ~~old~~ with $x$\n- [ ] pending\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let document = Document::parse(source.to_owned());

        let text_segments: Vec<_> = document
            .segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text { text, source, .. } => Some((text, source)),
                Segment::Break { .. }
                | Segment::Rule { .. }
                | Segment::Image { .. }
                | Segment::Heading { .. }
                | Segment::Table { .. }
                | Segment::Math { .. }
                | Segment::Link { .. }
                | Segment::CodeBlock { .. } => None,
            })
            .collect();

        let task_markers: Vec<_> = text_segments
            .iter()
            .filter(|(text, _)| text == &"☑" || text == &"☐")
            .collect();
        assert_eq!(task_markers.len(), 2);
        assert_eq!(task_markers[0].0, "☑");
        assert_eq!(&source[task_markers[0].1.clone()], "[x]");
        assert_eq!(task_markers[1].0, "☐");
        assert_eq!(&source[task_markers[1].1.clone()], "[ ]");
        assert!(text_segments.iter().all(|(text, _)| text != &"• "));

        for (index, segment) in document.segments.iter().enumerate() {
            let Segment::Text { text, .. } = segment else {
                continue;
            };
            if text != "☑" && text != "☐" {
                continue;
            }
            assert!(matches!(
                document.segments.get(index + 1),
                Some(Segment::Text {
                    text,
                    synthetic: true,
                    ..
                }) if text == "  "
            ));
        }
        assert_eq!(document.math().len(), 1);
        assert_eq!(document.math()[0].expression, "x");
        assert_eq!(document.math()[0].form, MathForm::Inline);
        assert_eq!(&source[document.math()[0].source.clone()], "$x$");
        assert_eq!(document.tables()[0].rows[0].cells[0].text, "a");
        assert_eq!(document.tables()[0].rows[1].cells[1].text, "2");
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
    fn inline_and_display_math_retain_exact_source_ranges_and_boundaries() {
        let source = "before $x^2 + y^2$ after\n\n$$\n\\frac{1}{3}\n$$\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.math().len(), 2);
        assert_eq!(document.math()[0].form, MathForm::Inline);
        assert_eq!(document.math()[0].expression, "x^2 + y^2");
        assert_eq!(&source[document.math()[0].source.clone()], "$x^2 + y^2$");
        assert_eq!(document.math()[1].form, MathForm::Display);
        assert_eq!(document.math()[1].expression, "\n\\frac{1}{3}\n");
        assert_eq!(
            &source[document.math()[1].source.clone()],
            "$$\n\\frac{1}{3}\n$$"
        );
        let before = document.segments.iter().find_map(|segment| match segment {
            Segment::Text { text, .. } if text == "before " => Some(text),
            _ => None,
        });
        let after = document.segments.iter().find_map(|segment| match segment {
            Segment::Text { text, .. } if text == " after" => Some(text),
            _ => None,
        });
        assert!(before.is_some());
        assert!(after.is_some());
    }

    #[test]
    fn tables_retain_alignment_rows_cells_and_source_ranges() {
        let source = "| **名前** | Value |\n| :--- | ---: |\n| 螺鈿 | `42` |\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.tables().len(), 1);
        let table = &document.tables()[0];
        assert_eq!(
            table.alignments,
            [TableAlignment::Left, TableAlignment::Right]
        );
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0].header);
        assert!(!table.rows[1].header);
        assert_eq!(table.rows[0].cells[0].text, "名前");
        assert_eq!(table.rows[1].cells[0].text, "螺鈿");
        assert_eq!(table.rows[1].cells[1].text, "`42`");
        assert_eq!(&source[table.source.clone()], source);
        for row in &table.rows {
            assert!(table.source.start <= row.source.start);
            assert!(row.source.end <= table.source.end);
            for cell in &row.cells {
                assert!(row.source.start <= cell.source.start);
                assert!(cell.source.end <= row.source.end);
                assert!(source[cell.source.clone()].contains(cell.text.trim_matches('`')));
            }
        }
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Table { index: 0 }))
        );
    }

    #[test]
    fn malformed_table_syntax_remains_reviewable_source_text() {
        let source = "| name | value |\n| missing delimiter row |\n";
        let document = Document::parse(source.to_owned());
        let rendered = document
            .segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert!(document.tables().is_empty());
        assert!(rendered.contains("name"));
        assert!(rendered.contains("missing delimiter row"));
    }

    #[test]
    fn fenced_code_blocks_retain_standard_info_string_language_and_source() {
        let source = "```rust linenos=true\nfn main() {\n    println!(\"hello\");\n}\n```\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.code_blocks().len(), 1);
        let code_block = &document.code_blocks()[0];
        assert_eq!(code_block.info.as_deref(), Some("rust linenos=true"));
        assert_eq!(code_block.language.as_deref(), Some("rust"));
        assert_eq!(
            code_block.text,
            "fn main() {\n    println!(\"hello\");\n}\n"
        );
        assert_eq!(&source[code_block.source.clone()], source.trim_end());
        assert_eq!(&source[code_block.content_source.clone()], code_block.text);
        assert!(code_block.style.code);
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::CodeBlock { index: 0 }))
        );
    }

    #[test]
    fn indented_and_language_free_blocks_do_not_invent_a_language() {
        let source = "```\nplain fenced\n```\n\n    indented\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.code_blocks().len(), 2);
        assert_eq!(document.code_blocks()[0].info.as_deref(), Some(""));
        assert_eq!(document.code_blocks()[0].language, None);
        assert_eq!(document.code_blocks()[1].info, None);
        assert_eq!(document.code_blocks()[1].language, None);
    }

    #[test]
    fn code_block_parts_retain_source_gaps_removed_by_markdown_parsing() {
        let source = "    abc\n    def\n";
        let document = Document::parse(source.to_owned());
        let code_block = &document.code_blocks()[0];

        assert_eq!(code_block.text, "abc\ndef\n");
        assert_eq!(code_block.parts.len(), 2);
        assert_eq!(&source[code_block.parts[0].source.clone()], "abc\n");
        assert_eq!(&source[code_block.parts[1].source.clone()], "def\n");
        assert_eq!(&code_block.text[code_block.parts[0].text.clone()], "abc\n");
        assert_eq!(&code_block.text[code_block.parts[1].text.clone()], "def\n");
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
    fn named_links_retain_label_destination_and_source_subranges() {
        let source = "See [the docs](https://example.com/a?b=c#d \"Docs\") today.\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.links().len(), 1);
        let link = &document.links()[0];
        assert_eq!(link.destination, "https://example.com/a?b=c#d");
        assert_eq!(link.label, "the docs");
        assert_eq!(link.title, "Docs");
        assert!(!link.is_collapsed());
        assert_eq!(
            link.destination_projection().as_deref(),
            Some(" <https://example.com/a?b=c#d>")
        );
        assert_eq!(
            &source[link.source.clone()],
            "[the docs](https://example.com/a?b=c#d \"Docs\")"
        );
        assert_eq!(&source[link.label_source.clone()], "the docs");
        assert_eq!(
            &source[link.destination_source.clone()],
            "https://example.com/a?b=c#d"
        );
        assert!(link.source.start <= link.label_source.start);
        assert!(link.label_source.end <= link.destination_source.start);
        assert!(link.destination_source.end <= link.source.end);
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Link { index: 0 }))
        );
    }

    #[test]
    fn autolinks_and_label_equal_destinations_are_not_duplicated() {
        let source = "<https://example.com> and [https://example.com](https://example.com) and <a@b.com> and [a@b.com](mailto:a@b.com)\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.links().len(), 4);
        for link in document.links() {
            assert!(link.is_collapsed());
            assert_eq!(link.destination_projection(), None);
        }
        assert_eq!(document.links()[2].destination, "a@b.com");
        assert_eq!(document.links()[3].destination, "mailto:a@b.com");
        assert_eq!(document.links()[3].label, "a@b.com");
        assert_eq!(
            &source[document.links()[0].destination_source.clone()],
            "https://example.com"
        );
    }

    #[test]
    fn relative_fragment_and_email_destinations_are_preserved_exactly() {
        let source = "[rel](../notes/a%20b.md) [frag](#section-1) [mail](mailto:a@b.com)\n";
        let document = Document::parse(source.to_owned());

        let destinations = document
            .links()
            .iter()
            .map(|link| link.destination.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            destinations,
            ["../notes/a%20b.md", "#section-1", "mailto:a@b.com"]
        );
        for link in document.links() {
            assert_eq!(&source[link.destination_source.clone()], link.destination);
        }
    }

    #[test]
    fn images_inside_links_stay_on_the_image_path() {
        let source = "[![diagram alt](assets/d.png)](https://example.com/page)\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.images().len(), 1);
        assert_eq!(document.images()[0].destination, "assets/d.png");
        assert_eq!(document.links().len(), 1);
        assert_eq!(document.links()[0].destination, "https://example.com/page");
        assert!(
            document
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Image { index: 0 }))
        );
    }

    #[test]
    fn reference_links_keep_a_meaningful_destination_subrange() {
        let source = "[label][ref]\n\n[ref]: https://example.com/target\n";
        let document = Document::parse(source.to_owned());

        assert_eq!(document.links().len(), 1);
        let link = &document.links()[0];
        assert_eq!(link.destination, "https://example.com/target");
        assert_eq!(link.label, "label");
        assert!(link.label_source.end <= link.destination_source.start);
        assert!(link.destination_source.end <= link.source.end);
        assert_eq!(&source[link.destination_source.clone()], "][ref]");
    }

    #[test]
    fn malformed_link_syntax_remains_readable_source_text() {
        let source = "[unclosed](https://example.com and [text] alone\n";
        let document = Document::parse(source.to_owned());
        let rendered = document
            .segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert!(document.links().is_empty());
        assert!(rendered.contains("[unclosed](https://example.com"));
        assert!(rendered.contains("[text]"));
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

    #[test]
    fn a_document_rule_is_a_width_independent_segment() {
        let source = "before\n\n---\n\nafter";
        let document = Document::parse(source.to_owned());

        let rules: Vec<_> = document
            .segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Rule { source, .. } => Some(source.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(rules.len(), 1);
        assert_eq!(source[rules[0].clone()].trim_end(), "---");
        assert!(
            !document.segments.iter().any(|segment| matches!(
                segment,
                Segment::Text { text, .. } if text.contains('─')
            )),
            "the parsed model must not resolve a rule into fixed glyph text"
        );
    }

    #[test]
    fn heading_summaries_stay_compact_around_a_rule() {
        let document = Document::parse("# Title\n\n---\n\n## Next\n".to_owned());

        let texts: Vec<_> = document
            .headings()
            .iter()
            .map(|heading| heading.text.as_str())
            .collect();

        assert_eq!(texts, ["Title", "Next"]);
    }

    #[test]
    fn a_table_cell_rule_stays_compact_source_text() {
        let document = Document::parse("| a |\n| - |\n| --- |\n".to_owned());

        assert_eq!(document.tables()[0].rows[1].cells[0].text, "---");
    }
}
