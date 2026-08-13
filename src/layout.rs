use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use unicode_width::UnicodeWidthChar;

use crate::document::{Document, Segment};
use crate::image::{ImageCatalog, MAX_IMAGE_ROWS};
use crate::{
    CellRect, DesiredFrame, ElementKey, GraphicPlacement, PixelSize, RasterAsset, RasterCache,
    SourceSpan, TextStyle,
};

const MAX_RASTERIZED_HEADINGS: usize = 8;
const MAX_LAYOUT_HEADING_BYTES: usize = 32 * 1024 * 1024;

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
    pub height: u16,
    graphic: Option<usize>,
}

#[derive(Clone, Debug)]
struct LayoutGraphic {
    element_key: ElementKey,
    source: SourceSpan,
    columns: u16,
    rows: u16,
    asset: Arc<RasterAsset>,
}

/// Width-dependent terminal layout derived from a source-ranged document.
#[derive(Clone, Debug)]
pub struct Layout {
    width: usize,
    rows: Vec<LayoutRow>,
    graphics: Vec<LayoutGraphic>,
}

impl Layout {
    #[must_use]
    pub fn new(document: &Document, width: u16) -> Self {
        Self::build(document, width, None, None)
    }

    pub(crate) fn with_text(document: &Document, width: u16, images: &ImageCatalog) -> Self {
        Self::build(document, width, Some(images), None)
    }

    pub(crate) fn with_graphics(
        document: &Document,
        width: u16,
        height: u16,
        images: &ImageCatalog,
        cache: &mut RasterCache,
        cell_pixels: PixelSize,
        focus_source: usize,
    ) -> Self {
        Self::build(
            document,
            width,
            Some(images),
            Some((cache, cell_pixels, height, focus_source)),
        )
    }

    fn build(
        document: &Document,
        width: u16,
        images: Option<&ImageCatalog>,
        mut graphics: Option<(&mut RasterCache, PixelSize, u16, usize)>,
    ) -> Self {
        let width = usize::from(width.max(1));
        let mut builder = Builder::new(width);
        let selected_headings = graphics
            .as_ref()
            .map(|(_, _, _, focus_source)| {
                let mut indices = document.headings().iter().enumerate().collect::<Vec<_>>();
                indices.sort_by_key(|(_, heading)| heading.source.start.abs_diff(*focus_source));
                indices
                    .into_iter()
                    .take(MAX_RASTERIZED_HEADINGS)
                    .map(|(index, _)| index)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let mut heading_bytes = 0usize;
        for segment in &document.segments {
            match segment {
                Segment::Text {
                    text,
                    source,
                    style,
                    synthetic,
                } => builder.text(text, source.clone(), *style, *synthetic),
                Segment::Break { source, blank } => builder.line_break(*source, *blank),
                Segment::Image { index } => {
                    let prepared = images.and_then(|catalog| catalog.get(*index));
                    let mut raster_error = None;
                    let rendered = match (prepared, graphics.as_mut()) {
                        (Some(image), Some((cache, cell_pixels, available_rows, _)))
                            if image.can_render() =>
                        {
                            match cache.rasterize(
                                image,
                                (
                                    u16::try_from(width).unwrap_or(u16::MAX),
                                    (*available_rows).clamp(1, MAX_IMAGE_ROWS),
                                ),
                                *cell_pixels,
                            ) {
                                Ok(Some((asset, columns, rows))) => {
                                    builder.image(
                                        image.element_key,
                                        image.source.clone(),
                                        columns,
                                        rows,
                                        asset,
                                    );
                                    true
                                }
                                Ok(None) => false,
                                Err(error) => {
                                    raster_error = Some(error);
                                    false
                                }
                            }
                        }
                        _ => false,
                    };
                    if !rendered {
                        let fallback = prepared
                            .map(|image| match raster_error.as_deref() {
                                Some(error) => image.failure_projection(error),
                                None => image.text_projection.clone(),
                            })
                            .or_else(|| {
                                document.images().get(*index).map(|image| {
                                    let label = if image.alt.trim().is_empty() {
                                        image.destination.as_str()
                                    } else {
                                        image.alt.trim()
                                    };
                                    format!("[image: {label}]")
                                })
                            })
                            .unwrap_or_else(|| "[image unavailable]".to_owned());
                        let source = prepared
                            .map(|image| image.source.clone())
                            .or_else(|| {
                                document
                                    .images()
                                    .get(*index)
                                    .map(|image| image.source.clone())
                            })
                            .unwrap_or(0..0);
                        let mut style = prepared
                            .map(|image| image.style)
                            .or_else(|| document.images().get(*index).map(|image| image.style))
                            .unwrap_or_default();
                        style.muted = true;
                        builder.text(&fallback, source, style, false);
                    }
                }
                Segment::Heading { index } => {
                    let Some(heading) = document.headings().get(*index) else {
                        continue;
                    };
                    let mut raster_error = None;
                    let rendered = match graphics.as_mut() {
                        Some((cache, cell_pixels, available_rows, _))
                            if selected_headings.contains(index) =>
                        {
                            match cache.rasterize_heading(
                                *index,
                                heading,
                                u16::try_from(width).unwrap_or(u16::MAX),
                                *available_rows,
                                *cell_pixels,
                            ) {
                                Ok(rendered) => rendered,
                                Err(error) => {
                                    raster_error = Some(error);
                                    None
                                }
                            }
                        }
                        Some(_) | None => None,
                    };
                    let rendered = rendered.filter(|rendered| {
                        let bytes = rendered.asset.rgba.len();
                        if heading_bytes.saturating_add(bytes) > MAX_LAYOUT_HEADING_BYTES {
                            false
                        } else {
                            heading_bytes = heading_bytes.saturating_add(bytes);
                            true
                        }
                    });
                    if let Some(rendered) = rendered {
                        builder.image(
                            rendered.element_key,
                            heading.source.clone(),
                            rendered.columns,
                            rendered.rows,
                            rendered.asset,
                        );
                    } else {
                        let fallback = raster_error.map_or_else(
                            || heading.text.clone(),
                            |error| {
                                format!("{} [heading raster unavailable: {error}]", heading.text)
                            },
                        );
                        builder.text(&fallback, heading.source.clone(), heading.style, false);
                    }
                }
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
    pub fn desired_frame(
        &self,
        first_row: usize,
        first_row_offset: u16,
        body: CellRect,
        generation: u64,
        cell_pixels: PixelSize,
    ) -> DesiredFrame {
        let mut placements = Vec::new();
        let mut assets = HashMap::new();
        let mut y = body.y;
        let bottom = body.y.saturating_add(body.height);
        for (visible_index, row) in self.rows.iter().skip(first_row).enumerate() {
            if y >= bottom {
                break;
            }
            let row_offset = if visible_index == 0 {
                first_row_offset.min(row.height.saturating_sub(1))
            } else {
                0
            };
            let remaining_height = row.height.saturating_sub(row_offset);
            if let Some(graphic) = row.graphic.and_then(|index| self.graphics.get(index)) {
                let visible_height = remaining_height.min(bottom.saturating_sub(y));
                let width = graphic.columns.min(body.width);
                let target = CellRect {
                    x: body.x,
                    y,
                    width: graphic.columns,
                    height: graphic.rows,
                };
                let clip = CellRect {
                    x: body.x,
                    y,
                    width,
                    height: visible_height,
                };
                placements.push(GraphicPlacement {
                    element_key: graphic.element_key,
                    asset_key: graphic.asset.key,
                    target,
                    clip,
                    source_y: u32::from(row_offset).saturating_mul(cell_pixels.height),
                    source: graphic.source.clone(),
                });
                assets.insert(graphic.asset.key, Arc::clone(&graphic.asset));
            }
            y = y.saturating_add(remaining_height).min(bottom);
        }
        DesiredFrame {
            generation,
            cell_pixels,
            placements,
            assets,
        }
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
    graphics: Vec<LayoutGraphic>,
}

impl Builder {
    fn new(width: usize) -> Self {
        Self {
            width,
            cell_width: 0,
            rows: Vec::new(),
            current: Vec::new(),
            anchor: 0,
            graphics: Vec::new(),
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

    fn image(
        &mut self,
        element_key: ElementKey,
        source: SourceSpan,
        columns: u16,
        rows: u16,
        asset: Arc<RasterAsset>,
    ) {
        if !self.current.is_empty() {
            self.push_row(source.start);
        }
        let graphic = self.graphics.len();
        self.graphics.push(LayoutGraphic {
            element_key,
            source: source.clone(),
            columns,
            rows,
            asset,
        });
        self.rows.push(LayoutRow {
            fragments: Vec::new(),
            source,
            height: rows,
            graphic: Some(graphic),
        });
        self.cell_width = 0;
    }

    fn push_row(&mut self, fallback: usize) {
        let source = row_source(&self.current).unwrap_or(fallback..fallback);
        self.rows.push(LayoutRow {
            fragments: std::mem::take(&mut self.current),
            source,
            height: 1,
            graphic: None,
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
                height: 1,
                graphic: None,
            });
        }
        Layout {
            width: self.width,
            rows: self.rows,
            graphics: self.graphics,
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use image::{ImageBuffer, Rgba};

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

    #[test]
    fn text_projection_is_used_without_an_image_catalog() {
        let document = Document::parse("![diagram](missing.png)".to_owned());
        let layout = Layout::new(&document, 80);
        let rendered = layout.rows()[0]
            .fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert_eq!(rendered, "[image: diagram]");
        assert_eq!(layout.rows()[0].source, 0..23);
    }

    #[test]
    fn prepared_text_projection_includes_an_actionable_failure() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-layout-text-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let document = Document::parse("![diagram](missing.png)".to_owned());
        let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
        let layout = Layout::with_text(&document, 120, &catalog);
        let rendered = layout.rows()[0]
            .fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert!(rendered.contains("image unavailable: diagram"));
        assert!(rendered.contains("could not open"));
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn text_projection_keeps_inline_flow_and_outer_style() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-layout-inline-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let document = Document::parse("**left ![diagram](missing.png) right**".to_owned());
        let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
        let layout = Layout::with_text(&document, 120, &catalog);
        let rendered = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        let fragments = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .collect::<Vec<_>>();

        assert!(rendered.starts_with("left [image unavailable: diagram — could not open "));
        assert!(rendered.ends_with("] right"));
        assert!(fragments.iter().all(|fragment| fragment.style.strong));
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn graphic_row_and_placement_keep_the_image_source_range() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-layout-image-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let image_path = root.join("image.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(4, 4, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("fixture should be saved");
        let markdown = "before\n\n![diagram](image.png)\n\nafter";
        let document = Document::parse(markdown.to_owned());
        let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
        let mut cache = RasterCache::new(1_024);
        let cell = PixelSize {
            width: 1,
            height: 1,
        };
        let layout = Layout::with_graphics(&document, 10, 10, &catalog, &mut cache, cell, 0);
        let image_source = document.images()[0].source.clone();
        let graphic_index = layout
            .rows()
            .iter()
            .position(|row| row.graphic.is_some())
            .expect("layout should contain graphic row");
        let graphic_row = &layout.rows()[graphic_index];
        let before = markdown.find("before").expect("fixture contains before");
        let after = markdown.find("after").expect("fixture contains after");
        let frame = layout.desired_frame(
            layout.row_for_source(image_source.start),
            0,
            CellRect {
                x: 0,
                y: 1,
                width: 10,
                height: 10,
            },
            1,
            cell,
        );

        assert_eq!(graphic_row.source, image_source);
        assert_eq!(frame.placements[0].source, image_source);
        let clipped = layout.desired_frame(
            graphic_index,
            1,
            CellRect {
                x: 0,
                y: 1,
                width: 10,
                height: 10,
            },
            2,
            cell,
        );
        assert_eq!(clipped.placements[0].source_y, 1);
        assert_eq!(clipped.placements[0].clip.height, 3);
        assert!(layout.row_for_source(before) < graphic_index);
        assert!(layout.row_for_source(after) > graphic_index);
        let narrow = Layout::with_graphics(&document, 3, 10, &catalog, &mut cache, cell, 0);
        assert!(
            narrow.rows()[narrow.row_for_source(after)]
                .source
                .contains(&after)
        );
        fs::remove_file(image_path).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn large_headings_render_as_graphics_and_fall_back_to_text() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-layout-heading-{suffix}"));
        fs::create_dir(&root).expect("fixture directory should be created");
        let markdown = "# 漆と螺鈿\n\n## A long second-level heading\n\n### Plain heading";
        let document = Document::parse(markdown.to_owned());
        let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let mut cache = RasterCache::default();
        let layout = Layout::with_graphics(&document, 24, 20, &catalog, &mut cache, cell, 0);
        let frame = layout.desired_frame(
            0,
            0,
            CellRect {
                x: 0,
                y: 0,
                width: 24,
                height: 20,
            },
            1,
            cell,
        );

        assert_eq!(frame.placements.len(), 2);
        assert_eq!(frame.placements[0].source, document.headings()[0].source);
        assert_eq!(frame.placements[1].source, document.headings()[1].source);
        assert_ne!(frame.placements[0].asset_key, frame.placements[1].asset_key);

        let text_layout = Layout::new(&document, 80);
        let rendered = text_layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("漆と螺鈿"));
        assert!(rendered.contains("A long second-level heading"));
        assert!(rendered.contains("Plain heading"));

        let mut undersized_cache = RasterCache::new(1);
        let diagnostic_layout =
            Layout::with_graphics(&document, 24, 20, &catalog, &mut undersized_cache, cell, 0);
        let diagnostic = diagnostic_layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        assert!(diagnostic.contains("漆と螺鈿"));
        assert!(diagnostic.contains("heading raster unavailable"));
        assert!(diagnostic.contains("cache capacity"));
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn only_headings_nearest_the_viewport_hold_raster_assets() {
        let markdown = (0..40)
            .map(|index| format!("# Heading {index}\n\nbody {index}\n\n"))
            .collect::<String>();
        let document = Document::parse(markdown.clone());
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let focus = markdown
            .find("Heading 20")
            .expect("fixture has focused heading");
        let mut cache = RasterCache::default();
        let layout = Layout::with_graphics(
            &document,
            80,
            30,
            &catalog,
            &mut cache,
            PixelSize {
                width: 8,
                height: 16,
            },
            focus,
        );
        let graphic_rows = layout
            .rows()
            .iter()
            .filter(|row| row.graphic.is_some())
            .count();

        assert_eq!(graphic_rows, MAX_RASTERIZED_HEADINGS);
        assert!(
            layout.rows()[layout.row_for_source(focus)]
                .graphic
                .is_some()
        );
        assert!(
            layout
                .graphics
                .iter()
                .map(|graphic| graphic.asset.rgba.len())
                .sum::<usize>()
                <= MAX_LAYOUT_HEADING_BYTES
        );
    }
}
