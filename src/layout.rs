use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use unicode_width::UnicodeWidthChar;

use crate::document::{Document, Segment};
use crate::image::{ImageCatalog, MAX_IMAGE_ROWS};
use crate::scene::GraphicSourceRegion;
use crate::{
    CellRect, DesiredFrame, ElementKey, GraphicPlacement, MathForm, PixelSize, RasterAsset,
    RasterCache, SourceSpan, TextStyle,
};

const MAX_RASTERIZED_HEADINGS: usize = 8;
const MAX_LAYOUT_HEADING_BYTES: usize = 32 * 1024 * 1024;
const MAX_RASTERIZED_TABLES: usize = 4;
const MAX_LAYOUT_TABLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_LAYOUT_MATH_BYTES: usize = 32 * 1024 * 1024;

/// The single-cell glyph a thematic break is drawn from.
const RULE_GLYPH: char = '─';

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
    graphics: Vec<usize>,
}

#[derive(Clone, Debug)]
struct LayoutGraphic {
    element_key: ElementKey,
    source: SourceSpan,
    columns: u16,
    rows: u16,
    asset: Arc<RasterAsset>,
    source_regions: Vec<GraphicSourceRegion>,
    column: u16,
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
        let selected_tables = graphics
            .as_ref()
            .map(|(_, _, _, focus_source)| {
                let mut indices = document.tables().iter().enumerate().collect::<Vec<_>>();
                indices.sort_by_key(|(_, table)| table.source.start.abs_diff(*focus_source));
                indices
                    .into_iter()
                    .take(MAX_RASTERIZED_TABLES)
                    .map(|(index, _)| index)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let mut heading_bytes = 0usize;
        let mut table_bytes = 0usize;
        let mut math_bytes = 0usize;
        for segment in &document.segments {
            match segment {
                Segment::Text {
                    text,
                    source,
                    style,
                    synthetic,
                } => builder.text(text, source.clone(), *style, *synthetic),
                Segment::Break { source, blank } => builder.line_break(*source, *blank),
                Segment::Rule { source, style } => builder.rule(source.clone(), *style),
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
                                        Vec::new(),
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
                            Vec::new(),
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
                Segment::Table { index } => {
                    let Some(table) = document.tables().get(*index) else {
                        continue;
                    };
                    let mut raster_error = None;
                    let rendered = match graphics.as_mut() {
                        Some((cache, cell_pixels, _, _)) if selected_tables.contains(index) => {
                            match cache.rasterize_table(
                                *index,
                                table,
                                u16::try_from(width).unwrap_or(u16::MAX),
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
                        match reserve_table_bytes(&mut table_bytes, bytes) {
                            Ok(()) => true,
                            Err(error) => {
                                raster_error = Some(error);
                                false
                            }
                        }
                    });
                    if let Some(rendered) = rendered {
                        builder.image(
                            rendered.element_key,
                            table.source.clone(),
                            rendered.columns,
                            rendered.rows,
                            rendered.asset,
                            rendered.source_regions,
                        );
                    } else {
                        builder.table_text(table, raster_error.as_deref());
                    }
                }
                Segment::Link { index } => {
                    let Some(link) = document.links().get(*index) else {
                        continue;
                    };
                    let Some(projection) = link.destination_projection() else {
                        continue;
                    };
                    let mut style = link.style;
                    style.link = false;
                    style.muted = true;
                    builder.text(&projection, link.destination_source.clone(), style, false);
                }
                Segment::Math { index } => {
                    let Some(math) = document.math().get(*index) else {
                        continue;
                    };
                    let mut raster_error = None;
                    let available_columns = width;
                    let rendered = match graphics.as_mut() {
                        Some((cache, cell_pixels, available_rows, _)) => {
                            match cache.rasterize_math(
                                *index,
                                math,
                                u16::try_from(available_columns).unwrap_or(u16::MAX),
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
                        None => None,
                    };
                    let rendered = rendered.filter(|rendered| {
                        let bytes = rendered.asset.rgba.len();
                        if math_bytes.saturating_add(bytes) > MAX_LAYOUT_MATH_BYTES {
                            raster_error = Some(format!(
                                "math raster needs {bytes} bytes but the layout budget has {} of {MAX_LAYOUT_MATH_BYTES} bytes remaining",
                                MAX_LAYOUT_MATH_BYTES.saturating_sub(math_bytes)
                            ));
                            false
                        } else {
                            math_bytes = math_bytes.saturating_add(bytes);
                            true
                        }
                    });
                    if let Some(rendered) = rendered {
                        match math.form {
                            MathForm::Inline => builder.inline_image(
                                rendered.element_key,
                                math.source.clone(),
                                math.style,
                                rendered.columns,
                                rendered.rows,
                                rendered.asset,
                            ),
                            MathForm::Display => builder.centered_image(
                                rendered.element_key,
                                math.source.clone(),
                                rendered.columns,
                                rendered.rows,
                                rendered.asset,
                            ),
                        }
                    } else {
                        let projection = raster_error.map_or_else(
                            || math.text_projection(),
                            |error| {
                                format!(
                                    "{} [math raster unavailable: {error}]",
                                    math.text_projection()
                                )
                            },
                        );
                        let mut style = math.style;
                        style.code = true;
                        builder.text(&projection, math.source.clone(), style, false);
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
            for graphic in row
                .graphics
                .iter()
                .filter_map(|index| self.graphics.get(*index))
            {
                if row_offset >= graphic.rows {
                    continue;
                }
                let graphic_remaining = graphic.rows.saturating_sub(row_offset);
                let visible_height = graphic_remaining.min(bottom.saturating_sub(y));
                let available_width = body.width.saturating_sub(graphic.column);
                let width = graphic.columns.min(available_width);
                let target = CellRect {
                    x: body.x.saturating_add(graphic.column),
                    y,
                    width: graphic.columns,
                    height: graphic.rows,
                };
                let clip = CellRect {
                    x: body.x.saturating_add(graphic.column),
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
                    source_regions: graphic
                        .source_regions
                        .iter()
                        .filter_map(|region| {
                            let bottom = region.target.y.saturating_add(region.target.height);
                            if bottom <= row_offset {
                                return None;
                            }
                            let top = region.target.y.max(row_offset);
                            Some(GraphicSourceRegion {
                                target: CellRect {
                                    x: region.target.x,
                                    y: top.saturating_sub(row_offset),
                                    width: region.target.width,
                                    height: bottom.saturating_sub(top),
                                },
                                source: region.source.clone(),
                            })
                        })
                        .collect(),
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
            .position(|row| row.source.start <= offset && offset < row.source.end)
            .or_else(|| self.rows.iter().position(|row| row.source.start >= offset))
            .unwrap_or_else(|| self.rows.len().saturating_sub(1))
    }

    pub(crate) fn row_for_anchor(&self, source: &SourceSpan) -> usize {
        self.rows
            .iter()
            .position(|row| row.source == *source)
            .unwrap_or_else(|| self.row_for_source(source.start))
    }
}

fn reserve_table_bytes(used: &mut usize, incoming: usize) -> Result<(), String> {
    if used.saturating_add(incoming) > MAX_LAYOUT_TABLE_BYTES {
        return Err(format!(
            "table raster needs {incoming} bytes but the layout budget has {} of {MAX_LAYOUT_TABLE_BYTES} bytes remaining",
            MAX_LAYOUT_TABLE_BYTES.saturating_sub(*used)
        ));
    }
    *used = used.saturating_add(incoming);
    Ok(())
}

#[derive(Debug)]
struct Builder {
    width: usize,
    cell_width: usize,
    rows: Vec<LayoutRow>,
    current: Vec<LayoutFragment>,
    anchor: usize,
    graphics: Vec<LayoutGraphic>,
    current_graphics: Vec<usize>,
    current_height: u16,
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
            current_graphics: Vec::new(),
            current_height: 1,
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

    /// Emit a thematic break spanning the content width still available on this row.
    fn rule(&mut self, source: SourceSpan, style: TextStyle) {
        if !self.current.is_empty() {
            self.push_row(source.start);
        }
        let columns = self.width.saturating_sub(self.cell_width).max(1);
        self.current.push(LayoutFragment {
            text: RULE_GLYPH.to_string().repeat(columns),
            source,
            style,
        });
        self.cell_width = self.cell_width.saturating_add(columns);
    }

    fn image(
        &mut self,
        element_key: ElementKey,
        source: SourceSpan,
        columns: u16,
        rows: u16,
        asset: Arc<RasterAsset>,
        source_regions: Vec<GraphicSourceRegion>,
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
            source_regions,
            column: 0,
        });
        self.rows.push(LayoutRow {
            fragments: Vec::new(),
            source,
            height: rows,
            graphics: vec![graphic],
        });
        self.cell_width = 0;
    }

    fn centered_image(
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
        let column =
            u16::try_from(self.width.saturating_sub(usize::from(columns)) / 2).unwrap_or(u16::MAX);
        self.graphics.push(LayoutGraphic {
            element_key,
            source: source.clone(),
            columns,
            rows,
            asset,
            source_regions: Vec::new(),
            column,
        });
        self.rows.push(LayoutRow {
            fragments: Vec::new(),
            source,
            height: rows,
            graphics: vec![graphic],
        });
        self.cell_width = 0;
    }

    fn inline_image(
        &mut self,
        element_key: ElementKey,
        source: SourceSpan,
        style: TextStyle,
        columns: u16,
        rows: u16,
        asset: Arc<RasterAsset>,
    ) {
        let columns_usize = usize::from(columns);
        if self.cell_width > 0 && self.cell_width.saturating_add(columns_usize) > self.width {
            self.push_row(source.start);
        }
        let column = u16::try_from(self.cell_width).unwrap_or(u16::MAX);
        let graphic = self.graphics.len();
        self.graphics.push(LayoutGraphic {
            element_key,
            source: source.clone(),
            columns,
            rows,
            asset,
            source_regions: Vec::new(),
            column,
        });
        self.current_graphics.push(graphic);
        self.current_height = self.current_height.max(rows);
        self.current.push(LayoutFragment {
            text: " ".repeat(columns_usize),
            source,
            style,
        });
        self.cell_width = self.cell_width.saturating_add(columns_usize);
    }

    fn table_text(&mut self, table: &crate::TableElement, raster_error: Option<&str>) {
        self.line_break(table.source.start, false);
        if let Some(error) = raster_error {
            let mut style = table.style;
            style.muted = true;
            self.text(
                &format!("[table raster unavailable: {error}]"),
                table.source.clone(),
                style,
                false,
            );
            self.push_row(table.source.start);
        }
        for row in &table.rows {
            self.synthetic_text("│ ", row.source.start..row.source.start, table.style);
            for (column, cell) in row.cells.iter().enumerate() {
                if column > 0 {
                    self.synthetic_text(" │ ", cell.source.start..cell.source.start, table.style);
                }
                let mut style = cell.style;
                if row.header {
                    style.strong = true;
                }
                self.text(&cell.text, cell.source.clone(), style, false);
            }
            self.synthetic_text(" │", row.source.end..row.source.end, table.style);
            self.push_row(row.source.end);
        }
        self.line_break(table.source.end, true);
    }

    fn push_row(&mut self, fallback: usize) {
        let source = row_source(&self.current).unwrap_or(fallback..fallback);
        self.rows.push(LayoutRow {
            fragments: std::mem::take(&mut self.current),
            source,
            height: self.current_height,
            graphics: std::mem::take(&mut self.current_graphics),
        });
        self.cell_width = 0;
        self.current_height = 1;
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
                graphics: Vec::new(),
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
    fn inline_math_shares_its_logical_row_and_keeps_exact_source_targeting() {
        let markdown = "before $x^2 + y^2$ after";
        let document = Document::parse(markdown.to_owned());
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::default();
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let layout = Layout::with_graphics(&document, 80, 20, &catalog, &mut cache, cell, 0);
        let row = layout
            .rows()
            .iter()
            .find(|row| !row.graphics.is_empty())
            .expect("inline math should produce a graphic on a text row");
        let text = row
            .fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        let frame = layout.desired_frame(
            layout.row_for_source(document.math()[0].source.start),
            0,
            CellRect {
                x: 0,
                y: 0,
                width: 80,
                height: 20,
            },
            1,
            cell,
        );

        assert!(text.starts_with("before "));
        assert!(text.ends_with(" after"));
        assert_eq!(frame.placements.len(), 1);
        assert!(frame.placements[0].target.x > 0);
        assert_eq!(frame.placements[0].source, document.math()[0].source);
        assert_eq!(
            frame.placements[0]
                .source_at(frame.placements[0].target.x, frame.placements[0].target.y),
            document.math()[0].source
        );
    }

    #[test]
    fn mixed_height_inline_graphics_clip_within_each_asset_when_scrolled() {
        let cell = PixelSize {
            width: 1,
            height: 1,
        };
        let asset = |seed: u8, height: u32| {
            let key = crate::RasterAssetKey {
                content_hash: [seed; 32],
                pixels: PixelSize { width: 2, height },
                rasterizer_version: 1,
            };
            Arc::new(RasterAsset {
                key,
                rgba: Arc::from(vec![seed; 2 * height as usize * 4]),
            })
        };
        let mut builder = Builder::new(20);
        builder.inline_image(ElementKey(1), 0..3, TextStyle::default(), 2, 1, asset(1, 1));
        builder.inline_image(ElementKey(2), 3..6, TextStyle::default(), 2, 2, asset(2, 2));
        let layout = builder.finish(6);
        let body = CellRect {
            x: 0,
            y: 0,
            width: 20,
            height: 2,
        };

        let full = layout.desired_frame(0, 0, body, 1, cell);
        assert_eq!(full.placements.len(), 2);
        assert_eq!(full.placements[0].clip.height, 1);
        assert_eq!(full.placements[1].clip.height, 2);

        let scrolled = layout.desired_frame(0, 1, body, 2, cell);
        assert_eq!(scrolled.placements.len(), 1);
        assert_eq!(scrolled.placements[0].element_key, ElementKey(2));
        assert_eq!(scrolled.placements[0].source_y, 1);
        assert_eq!(scrolled.placements[0].clip.height, 1);
    }

    #[test]
    fn display_math_is_centered_and_invalid_math_falls_back_to_source() {
        let supported = Document::parse("$$\\frac{1}{3}$$".to_owned());
        let catalog = ImageCatalog::load(&supported, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::default();
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let layout = Layout::with_graphics(&supported, 80, 20, &catalog, &mut cache, cell, 0);
        let frame = layout.desired_frame(
            0,
            0,
            CellRect {
                x: 0,
                y: 0,
                width: 80,
                height: 20,
            },
            1,
            cell,
        );
        let placement = frame
            .placements
            .first()
            .expect("display math should produce a placement");
        assert_eq!(
            placement.target.x,
            (80_u16.saturating_sub(placement.target.width)) / 2
        );

        let invalid = Document::parse("before $\\unknowncommand{x}$ after".to_owned());
        let invalid_catalog = ImageCatalog::load(&invalid, std::path::Path::new("doc.md"));
        let invalid_layout =
            Layout::with_graphics(&invalid, 80, 20, &invalid_catalog, &mut cache, cell, 0);
        let projection = invalid_layout
            .rows()
            .iter()
            .flat_map(|row| &row.fragments)
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        assert!(projection.contains("$\\unknowncommand{x}$"));
        assert!(projection.contains("math raster unavailable"));
    }

    #[test]
    fn supported_math_is_not_silently_deferred_after_thirty_two_elements() {
        let markdown = (0..33)
            .map(|index| format!("$x_{{{index}}}$"))
            .collect::<Vec<_>>()
            .join(" ");
        let document = Document::parse(markdown);
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::default();
        let layout = Layout::with_graphics(
            &document,
            80,
            20,
            &catalog,
            &mut cache,
            PixelSize {
                width: 8,
                height: 16,
            },
            0,
        );

        assert_eq!(document.math().len(), 33);
        assert_eq!(layout.graphics.len(), 33);
        assert!(
            layout
                .rows()
                .iter()
                .flat_map(|row| &row.fragments)
                .all(|fragment| !fragment.text.contains("math raster unavailable"))
        );
    }

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
    fn link_labels_are_followed_by_a_subdued_destination() {
        let document = Document::parse("Read [the docs](https://example.com/a) now.".to_owned());
        let layout = Layout::new(&document, 80);
        let fragments = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .collect::<Vec<_>>();
        let rendered = fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert_eq!(rendered, "Read the docs <https://example.com/a> now.");
        assert!(
            fragments
                .iter()
                .any(|fragment| fragment.text.contains("the docs") && fragment.style.link)
        );
        let destination = fragments
            .iter()
            .find(|fragment| fragment.text.contains("example.com"))
            .expect("destination is rendered");
        assert!(destination.style.muted);
        assert!(!destination.style.link);
        assert!(
            document.links()[0]
                .destination_source
                .contains(&destination.source.start)
        );
    }

    #[test]
    fn collapsed_links_render_their_destination_once() {
        let document = Document::parse("<https://example.com/a> then more".to_owned());
        let rendered = Layout::new(&document, 80)
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert_eq!(rendered, "https://example.com/a then more");
    }

    #[test]
    fn long_and_unicode_destinations_wrap_without_clipping_or_reordering() {
        let markdown =
            "[螺鈿の資料](https://example.com/very/long/path/segment/for/wrapping/日本語)";
        let document = Document::parse(markdown.to_owned());
        let layout = Layout::new(&document, 16);
        let rendered = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert_eq!(
            rendered.replace(' ', ""),
            "螺鈿の資料<https://example.com/very/long/path/segment/for/wrapping/日本語>"
                .replace(' ', "")
        );
        assert!(layout.rows().len() > 1);
        assert!(layout.rows().iter().all(|row| {
            row.fragments
                .iter()
                .map(|fragment| {
                    fragment
                        .text
                        .chars()
                        .map(|character| character.width().unwrap_or(0))
                        .sum::<usize>()
                })
                .sum::<usize>()
                <= layout.width()
        }));
        assert!(
            layout
                .rows()
                .windows(2)
                .all(|rows| rows[0].source.start <= rows[1].source.start)
        );
        assert!(layout.rows().iter().all(|row| {
            row.fragments.iter().all(|fragment| {
                fragment.source.start <= fragment.source.end
                    && fragment.source.end <= document.source_len()
            })
        }));
    }

    #[test]
    fn linked_images_keep_the_image_projection_and_the_link_destination() {
        let document =
            Document::parse("[![diagram](missing.png)](https://example.com/page)".to_owned());
        let rendered = Layout::new(&document, 80)
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert_eq!(rendered, "[image: diagram] <https://example.com/page>");
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
    fn narrow_table_fallback_keeps_every_cell_and_its_source_span() {
        let markdown = "| Name | 説明 |\n| --- | --- |\n| raden | very long Unicode 内容 |\n";
        let document = Document::parse(markdown.to_owned());
        let layout = Layout::new(&document, 12);
        let fragments = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .collect::<Vec<_>>();
        let rendered = fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        for cell in document.tables()[0]
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
        {
            assert!(rendered.contains(&cell.text));
            assert!(fragments.iter().any(|fragment| {
                !fragment.source.is_empty()
                    && cell.source.start <= fragment.source.start
                    && fragment.source.end <= cell.source.end
            }));
        }
    }

    #[test]
    fn table_raster_uses_the_scene_cache_and_changes_on_resize() {
        let markdown = "before\n\n| Name | 説明 | Value |\n| :--- | :---: | ---: |\n| raden | Unicode 螺鈿 with wrapped content | 42 |\n\nafter";
        let document = Document::parse(markdown.to_owned());
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::default();
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let layout = Layout::with_graphics(&document, 48, 20, &catalog, &mut cache, cell, 0);
        let table_source = document.tables()[0].source.clone();
        let frame = layout.desired_frame(
            layout.row_for_source(table_source.start),
            0,
            CellRect {
                x: 0,
                y: 0,
                width: 48,
                height: 20,
            },
            1,
            cell,
        );
        assert_eq!(frame.placements.len(), 1);
        assert_eq!(frame.placements[0].source, table_source);
        assert_eq!(frame.placements[0].source_regions.len(), 6);
        assert_eq!(
            frame.placements[0].source_at(1, 0),
            document.tables()[0].rows[0].cells[0].source
        );
        let last_region = frame.placements[0]
            .source_regions
            .last()
            .expect("body cell region exists");
        assert_eq!(
            frame.placements[0].source_at(last_region.target.x, last_region.target.y),
            document.tables()[0].rows[1].cells[2].source
        );
        let body_region = frame.placements[0].source_regions[3].clone();
        let scrolled = layout.desired_frame(
            layout.row_for_source(table_source.start),
            body_region.target.y,
            CellRect {
                x: 0,
                y: 0,
                width: 48,
                height: 20,
            },
            2,
            cell,
        );
        assert_eq!(
            scrolled.placements[0].source_at(body_region.target.x, 0),
            document.tables()[0].rows[1].cells[0].source
        );
        assert!(frame.placements[0].target.height > 1);
        assert!(
            frame.assets[&frame.placements[0].asset_key]
                .rgba
                .iter()
                .any(|channel| *channel != 255)
        );

        let cached = Layout::with_graphics(&document, 48, 20, &catalog, &mut cache, cell, 0)
            .desired_frame(
                layout.row_for_source(table_source.start),
                0,
                CellRect {
                    x: 0,
                    y: 0,
                    width: 48,
                    height: 20,
                },
                2,
                cell,
            );
        assert_eq!(
            cached.placements[0].asset_key,
            frame.placements[0].asset_key
        );

        let resized = Layout::with_graphics(&document, 64, 20, &catalog, &mut cache, cell, 0)
            .desired_frame(
                layout.row_for_source(table_source.start),
                0,
                CellRect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 20,
                },
                3,
                cell,
            );
        assert_eq!(
            resized.placements[0].asset_key,
            frame.placements[0].asset_key
        );

        let narrowed = Layout::with_graphics(&document, 40, 20, &catalog, &mut cache, cell, 0)
            .desired_frame(
                layout.row_for_source(table_source.start),
                0,
                CellRect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 20,
                },
                4,
                cell,
            );
        assert_ne!(
            narrowed.placements[0].asset_key,
            frame.placements[0].asset_key
        );
    }

    #[test]
    fn table_raster_failure_falls_back_without_losing_content() {
        let markdown = "| Name | Value |\n| --- | ---: |\n| raden | 42 |\n";
        let document = Document::parse(markdown.to_owned());
        let catalog = ImageCatalog::load(&document, std::path::Path::new("doc.md"));
        let mut cache = RasterCache::new(1);
        let layout = Layout::with_graphics(
            &document,
            40,
            20,
            &catalog,
            &mut cache,
            PixelSize::FALLBACK,
            0,
        );
        let rendered = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert!(layout.graphics.is_empty());
        assert!(rendered.contains("table raster unavailable"));
        assert!(rendered.contains("cache capacity"));
        assert!(rendered.contains("Name"));
        assert!(rendered.contains("raden"));
        assert!(rendered.contains("42"));
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
            .position(|row| !row.graphics.is_empty())
            .expect("layout should contain graphic row");
        let graphic_row = &layout.rows()[graphic_index];
        let before = markdown.find("before").expect("fixture contains before");
        let after = markdown.find("after").expect("fixture contains after");
        let frame = layout.desired_frame(
            layout.row_for_source(image_source.start),
            0,
            CellRect {
                x: 7,
                y: 1,
                width: 10,
                height: 10,
            },
            1,
            cell,
        );

        assert_eq!(graphic_row.source, image_source);
        assert_eq!(frame.placements[0].source, image_source);
        assert_eq!(frame.placements[0].target.x, 7);
        assert_eq!(frame.placements[0].clip.x, 7);
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
            .filter(|row| !row.graphics.is_empty())
            .count();

        assert_eq!(graphic_rows, MAX_RASTERIZED_HEADINGS);
        assert!(
            !layout.rows()[layout.row_for_source(focus)]
                .graphics
                .is_empty()
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

    #[test]
    fn table_layout_budget_accepts_the_boundary_and_rejects_the_next_byte() {
        let mut used = 0;
        assert!(reserve_table_bytes(&mut used, MAX_LAYOUT_TABLE_BYTES).is_ok());
        assert_eq!(used, MAX_LAYOUT_TABLE_BYTES);
        assert!(reserve_table_bytes(&mut used, 1).is_err());
        assert_eq!(used, MAX_LAYOUT_TABLE_BYTES);
    }

    fn rule_row(layout: &Layout) -> &LayoutRow {
        layout
            .rows()
            .iter()
            .find(|row| {
                row.fragments
                    .iter()
                    .any(|fragment| fragment.text.starts_with(RULE_GLYPH))
            })
            .expect("a thematic break must produce a row")
    }

    fn rule_width(layout: &Layout) -> usize {
        rule_row(layout)
            .fragments
            .iter()
            .map(|fragment| fragment.text.chars().count())
            .sum()
    }

    #[test]
    fn a_root_thematic_rule_spans_the_content_width() {
        let source = "before\n\n---\n\nafter";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 40);

        let row = rule_row(&layout);
        assert_eq!(row.fragments.len(), 1);
        assert_eq!(row.fragments[0].text, RULE_GLYPH.to_string().repeat(40));
        assert_eq!(
            &source[row.source.clone()].trim_end(),
            &"---",
            "the row must keep the exact Markdown source range"
        );
    }

    #[test]
    fn a_nested_thematic_rule_spans_its_available_content_width() {
        for source in ["> quoted\n>\n> ---\n", "- item\n\n  ---\n"] {
            let document = Document::parse(source.to_owned());
            let layout = Layout::new(&document, 24);

            assert_eq!(
                rule_width(&layout),
                24,
                "nested rule in {source:?} must fill the available width"
            );
        }
    }

    #[test]
    fn a_thematic_rule_follows_the_body_width_on_resize() {
        let document = Document::parse("---\n".to_owned());

        for width in [1u16, 3, 20, 79, 200] {
            let layout = Layout::new(&document, width);
            assert_eq!(rule_width(&layout), usize::from(width));
        }
    }

    #[test]
    fn a_narrow_thematic_rule_stays_a_nonzero_separator() {
        let document = Document::parse("---\n".to_owned());
        let layout = Layout::new(&document, 0);

        assert_eq!(
            rule_width(&layout),
            1,
            "a degenerate width must still render one separator cell"
        );
    }

    #[test]
    fn thematic_rule_rows_keep_ordered_source_ranges() {
        let source = "before\n\n---\n\nafter\n\n***\n\nend";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 30);

        let mut previous = 0usize;
        for row in layout.rows() {
            assert!(row.source.start <= row.source.end);
            assert!(row.source.end <= source.len());
            assert!(
                row.source.start >= previous,
                "row sources must not move backwards"
            );
            previous = row.source.start;
        }
        assert_eq!(
            layout
                .rows()
                .iter()
                .filter(|row| row
                    .fragments
                    .iter()
                    .any(|fragment| fragment.text.starts_with(RULE_GLYPH)))
                .count(),
            2
        );
    }

    #[test]
    fn a_table_cell_rule_stays_compact_instead_of_spanning_the_document() {
        let source = "| a | b |\n| - | - |\n| --- | x |\n";
        let document = Document::parse(source.to_owned());
        let layout = Layout::new(&document, 60);

        assert_eq!(document.tables()[0].rows[1].cells[0].text, "---");
        assert!(
            layout
                .rows()
                .iter()
                .all(|row| row.fragments.iter().all(|fragment| fragment
                    .text
                    .chars()
                    .filter(|c| *c == RULE_GLYPH)
                    .count()
                    < 60)),
            "a rule inside a table must stay readable cell text"
        );
    }
}
