use std::sync::OnceLock;

use fontdue::{Font, FontSettings};
use image::RgbaImage;
use typst::layout::Abs;
use typst_as_lib::TypstEngine;
use unicode_width::UnicodeWidthStr;

use crate::{
    CellRect, ElementKey, GraphicSourceRegion, PixelSize, RasterAssetKey, TableAlignment,
    TableElement,
    image::{cell_aligned_size, pad_to_cell_grid},
};

const FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansJP.ttf");
static GLYPH_FONT: OnceLock<Result<Font, String>> = OnceLock::new();
const TABLE_FONT_POINTS: f32 = 10.0;
const TABLE_HORIZONTAL_INSET_POINTS: f32 = 5.0;
const TABLE_VERTICAL_INSET_POINTS: f32 = 3.0;
const TABLE_RULE_GAP_POINTS: f32 = 5.0;
const MIN_COLUMN_CELLS: usize = 7;
const MAX_COLUMN_WEIGHT: usize = 32;
const MAX_TABLE_COLUMNS: usize = 32;
const MAX_TABLE_CELLS: usize = 2_048;
const MAX_TABLE_RASTER_ROWS: u32 = 256;
const MAX_TABLE_RASTER_BYTES: u64 = 128 * 1024 * 1024;
const TABLE_RASTERIZER_VERSION: u16 = 10;

#[derive(Clone, Debug)]
pub(crate) struct TablePlan {
    pub key: RasterAssetKey,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
    source: String,
}

impl TablePlan {
    pub(crate) fn build(
        index: usize,
        table: &TableElement,
        available_columns: u16,
        cell_pixels: PixelSize,
    ) -> Result<Self, String> {
        validate_table(table)?;
        validate_glyphs(table)?;
        let columns = table.alignments.len().max(
            table
                .rows
                .iter()
                .map(|row| row.cells.len())
                .max()
                .unwrap_or(0),
        );
        let required = columns
            .saturating_mul(MIN_COLUMN_CELLS)
            .saturating_add(columns.saturating_add(1));
        if usize::from(available_columns) < required {
            return Err(format!(
                "table needs at least {required} columns for {columns} readable columns; viewport has {}",
                available_columns.max(1)
            ));
        }

        let widths = column_widths(table, columns, usize::from(available_columns));
        let table_columns = widths.iter().sum::<usize>().max(1);
        let target_width = u32::try_from(table_columns)
            .unwrap_or(u32::MAX)
            .saturating_mul(cell_pixels.width.max(1));
        let source = typst_source(table, &widths, target_width, cell_pixels)?;
        let rows = u16::try_from(
            logical_row_heights(table, &widths, cell_pixels)
                .into_iter()
                .sum::<usize>()
                .saturating_add(1)
                .saturating_add(rule_gap_rows(cell_pixels)),
        )
        .unwrap_or(u16::MAX);
        let content_hash = *blake3::hash(source.as_bytes()).as_bytes();
        Ok(Self {
            key: RasterAssetKey {
                content_hash,
                pixels: PixelSize {
                    width: target_width,
                    height: 0,
                },
                rasterizer_version: TABLE_RASTERIZER_VERSION,
            },
            element_key: Self::element_key(index),
            columns: u16::try_from(table_columns).unwrap_or(u16::MAX),
            rows,
            source,
        })
    }

    pub(crate) fn cache_identity(
        table: &TableElement,
        available_columns: u16,
        cell_pixels: PixelSize,
    ) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&TABLE_RASTERIZER_VERSION.to_le_bytes());
        hasher.update(&available_columns.to_le_bytes());
        hasher.update(&cell_pixels.width.to_le_bytes());
        hasher.update(&cell_pixels.height.to_le_bytes());
        for alignment in &table.alignments {
            hasher.update(&[*alignment as u8]);
        }
        for row in &table.rows {
            hasher.update(&[u8::from(row.header)]);
            for cell in &row.cells {
                hasher.update(cell.text.as_bytes());
                hasher.update(&[0]);
            }
            hasher.update(&[0xff]);
        }
        *hasher.finalize().as_bytes()
    }

    pub(crate) fn element_key(index: usize) -> ElementKey {
        ElementKey((1_u64 << 62) | (index as u64 + 1))
    }

    pub(crate) fn source_regions(
        table: &TableElement,
        rendered_columns: u16,
        rendered_rows: u16,
        cell_pixels: PixelSize,
    ) -> Vec<GraphicSourceRegion> {
        let columns = table.alignments.len().max(
            table
                .rows
                .iter()
                .map(|row| row.cells.len())
                .max()
                .unwrap_or(0),
        );
        if columns == 0 || table.rows.is_empty() {
            return Vec::new();
        }
        let widths = column_widths(table, columns, usize::from(rendered_columns.max(1)));
        let logical_heights = logical_row_heights(table, &widths, cell_pixels);
        let logical_total = logical_heights.iter().sum::<usize>().max(1);
        let mut regions = Vec::new();
        let mut logical_y = 0usize;
        for (row, row_height) in table.rows.iter().zip(logical_heights) {
            let y = scale_cells(logical_y, logical_total, rendered_rows);
            logical_y = logical_y.saturating_add(row_height);
            let bottom = scale_cells(logical_y, logical_total, rendered_rows);
            let mut x = 0u16;
            for (column, cell) in row.cells.iter().enumerate() {
                let width = u16::try_from(widths.get(column).copied().unwrap_or(1))
                    .unwrap_or(u16::MAX)
                    .min(rendered_columns.saturating_sub(x));
                regions.push(GraphicSourceRegion {
                    target: CellRect {
                        x,
                        y,
                        width,
                        height: bottom.saturating_sub(y).max(1),
                    },
                    source: cell.source.clone(),
                });
                x = x.saturating_add(width);
            }
        }
        regions
    }

    pub(crate) fn render(mut self, cell_pixels: PixelSize) -> Result<(Self, RgbaImage), String> {
        let engine = TypstEngine::builder()
            .main_file(self.source.clone())
            .fonts([FONT])
            .build();
        let warned = engine.compile();
        if !warned.warnings.is_empty() {
            return Err(format!(
                "Typst table compilation produced warnings: {}",
                warned
                    .warnings
                    .iter()
                    .map(|warning| warning.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let document = warned
            .output
            .map_err(|error| format!("Typst table compilation failed: {error}"))?;
        let svg = typst_svg::svg_merged(&document, &Default::default(), Abs::zero());
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
            .map_err(|error| format!("Typst SVG could not be parsed: {error}"))?;
        let natural = tree.size();
        let height = u32::from(self.rows).saturating_mul(cell_pixels.height.max(1));
        let scale = height as f32 / natural.height().max(1.0);
        let content_width = (natural.width() * scale).ceil().max(1.0) as u32;
        validate_raster_bounds(content_width, height, cell_pixels)?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(content_width, height)
            .ok_or_else(|| "table raster has invalid dimensions".to_owned())?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let raster =
            RgbaImage::from_raw(pixmap.width(), pixmap.height(), pixmap.take_demultiplied())
                .ok_or_else(|| {
                    "table raster byte length did not match its dimensions".to_owned()
                })?;
        let pixels = cell_aligned_size(
            PixelSize {
                width: content_width,
                height,
            },
            cell_pixels,
        );
        validate_raster_bounds(pixels.width, pixels.height, cell_pixels)?;
        let raster = pad_to_cell_grid(raster, cell_pixels);
        self.key.pixels = pixels;
        self.columns = cells_for(pixels.width, cell_pixels.width);
        Ok((self, raster))
    }
}

fn logical_row_heights(
    table: &TableElement,
    widths: &[usize],
    cell_pixels: PixelSize,
) -> Vec<usize> {
    table
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    wrap_text(
                        &cell.text,
                        content_width_pixels(widths.get(column).copied().unwrap_or(1), cell_pixels),
                    )
                    .map_or(1, |lines| lines.len())
                })
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect()
}

fn rule_gap_rows(cell_pixels: PixelSize) -> usize {
    let extra_points = (TABLE_RULE_GAP_POINTS - TABLE_VERTICAL_INSET_POINTS).max(0.0) * 4.0;
    let extra_pixels = extra_points * 96.0 / 72.0;
    (extra_pixels / cell_pixels.height.max(1) as f32).ceil() as usize
}

fn validate_raster_bounds(width: u32, height: u32, cell_pixels: PixelSize) -> Result<(), String> {
    let max_height = cell_pixels
        .height
        .max(1)
        .saturating_mul(MAX_TABLE_RASTER_ROWS);
    let bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if height > max_height || bytes > MAX_TABLE_RASTER_BYTES {
        return Err(format!(
            "table raster would be {width}x{height} ({bytes} bytes); limits are {MAX_TABLE_RASTER_ROWS} rows and {MAX_TABLE_RASTER_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_table(table: &TableElement) -> Result<(), String> {
    let columns = table.alignments.len().max(
        table
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0),
    );
    let cells = table.rows.iter().map(|row| row.cells.len()).sum::<usize>();
    if columns == 0 || table.rows.is_empty() {
        return Err("table has no cells".to_owned());
    }
    if columns > MAX_TABLE_COLUMNS || cells > MAX_TABLE_CELLS {
        return Err(format!(
            "table has {columns} columns and {cells} cells; limits are {MAX_TABLE_COLUMNS} columns and {MAX_TABLE_CELLS} cells"
        ));
    }
    Ok(())
}

fn table_font() -> Result<&'static Font, String> {
    GLYPH_FONT
        .get_or_init(|| {
            Font::from_bytes(FONT, FontSettings::default())
                .map_err(|error| format!("embedded table font could not be loaded: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn validate_glyphs(table: &TableElement) -> Result<(), String> {
    let font = table_font()?;
    if let Some(character) = table
        .rows
        .iter()
        .flat_map(|row| &row.cells)
        .flat_map(|cell| cell.text.chars())
        .find(|character| {
            !character.is_control() && !character.is_whitespace() && !font.has_glyph(*character)
        })
    {
        return Err(format!(
            "embedded table font has no glyph for U+{:04X}",
            u32::from(character)
        ));
    }
    Ok(())
}

fn column_widths(table: &TableElement, columns: usize, available: usize) -> Vec<usize> {
    let weights = (0..columns)
        .map(|column| {
            table
                .rows
                .iter()
                .filter_map(|row| row.cells.get(column))
                .map(|cell| UnicodeWidthStr::width(cell.text.as_str()))
                .max()
                .unwrap_or(MIN_COLUMN_CELLS)
                .clamp(MIN_COLUMN_CELLS, MAX_COLUMN_WEIGHT)
        })
        .collect::<Vec<_>>();
    let mut widths = weights;
    let used = widths.iter().sum::<usize>();
    if used > available {
        let mut excess = used - available;
        while excess > 0 {
            let Some((index, _)) = widths
                .iter()
                .enumerate()
                .filter(|(_, width)| **width > MIN_COLUMN_CELLS)
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[index] -= 1;
            excess -= 1;
        }
    }
    widths
}

fn typst_source(
    table: &TableElement,
    widths: &[usize],
    target_width: u32,
    cell_pixels: PixelSize,
) -> Result<String, String> {
    let page_points = f64::from(target_width) * 72.0 / 96.0;
    let total_width = widths.iter().sum::<usize>().max(1);
    let columns = widths
        .iter()
        .map(|width| format!("{:.4}pt", page_points * *width as f64 / total_width as f64))
        .collect::<Vec<_>>()
        .join(", ");
    let alignments = (0..widths.len())
        .map(
            |index| match table.alignments.get(index).copied().unwrap_or_default() {
                TableAlignment::Default | TableAlignment::Left => "left + top",
                TableAlignment::Center => "center + top",
                TableAlignment::Right => "right + top",
            },
        )
        .collect::<Vec<_>>()
        .join(", ");
    let last_row = table.rows.len().saturating_sub(1);
    let mut source = format!(
        "#set page(width: {page_points:.4}pt, height: auto, margin: 0pt, fill: none)\n#set text(font: \"Noto Sans JP\", size: {TABLE_FONT_POINTS}pt, fill: rgb(\"#d8dee9\"))\n#table(columns: ({columns}), align: ({alignments}), inset: (_, y) => if y == 0 {{ (left: {TABLE_HORIZONTAL_INSET_POINTS}pt, right: {TABLE_HORIZONTAL_INSET_POINTS}pt, top: {TABLE_RULE_GAP_POINTS}pt, bottom: {TABLE_RULE_GAP_POINTS}pt) }} else if y == 1 and y == {last_row} {{ (left: {TABLE_HORIZONTAL_INSET_POINTS}pt, right: {TABLE_HORIZONTAL_INSET_POINTS}pt, top: {TABLE_RULE_GAP_POINTS}pt, bottom: {TABLE_RULE_GAP_POINTS}pt) }} else if y == 1 {{ (left: {TABLE_HORIZONTAL_INSET_POINTS}pt, right: {TABLE_HORIZONTAL_INSET_POINTS}pt, top: {TABLE_RULE_GAP_POINTS}pt, bottom: {TABLE_VERTICAL_INSET_POINTS}pt) }} else if y == {last_row} {{ (left: {TABLE_HORIZONTAL_INSET_POINTS}pt, right: {TABLE_HORIZONTAL_INSET_POINTS}pt, top: {TABLE_VERTICAL_INSET_POINTS}pt, bottom: {TABLE_RULE_GAP_POINTS}pt) }} else {{ (x: {TABLE_HORIZONTAL_INSET_POINTS}pt, y: {TABLE_VERTICAL_INSET_POINTS}pt) }}, stroke: (_, y) => if y == 0 {{ (top: 1.4pt + rgb(\"#8290a5\"), bottom: 0.6pt + rgb(\"#8290a5\")) }} else if y == {last_row} {{ (bottom: 1.4pt + rgb(\"#8290a5\")) }} else {{ none }}, fill: none,\n"
    );
    for row in &table.rows {
        for (column, width) in widths.iter().copied().enumerate() {
            let text = row
                .cells
                .get(column)
                .map(|cell| cell.text.as_str())
                .unwrap_or_default();
            let weight = if row.header { "bold" } else { "regular" };
            source.push_str("  [");
            for (line_index, line) in wrap_text(text, content_width_pixels(width, cell_pixels))?
                .iter()
                .enumerate()
            {
                if line_index > 0 {
                    source.push_str("#linebreak()");
                }
                source.push_str(&format!(
                    "#box[#text(weight: \"{weight}\", \"{}\")]",
                    escape_typst_string(line)
                ));
            }
            source.push_str("],\n");
        }
    }
    source.push_str(")\n");
    Ok(source)
}

fn content_width_pixels(column_cells: usize, cell_pixels: PixelSize) -> f32 {
    let inset_pixels = TABLE_HORIZONTAL_INSET_POINTS * 2.0 * 96.0 / 72.0;
    (column_cells.saturating_mul(cell_pixels.width.max(1) as usize) as f32 - inset_pixels).max(1.0)
}

fn wrap_text(text: &str, width_pixels: f32) -> Result<Vec<String>, String> {
    let font = table_font()?;
    let font_pixels = TABLE_FONT_POINTS * 96.0 / 72.0;
    let mut lines = vec![String::new()];
    let mut used = 0.0_f32;
    let mut previous = None;
    for character in text.chars() {
        let character_width = font.metrics(character, font_pixels).advance_width
            + previous
                .and_then(|before| font.horizontal_kern(before, character, font_pixels))
                .unwrap_or(0.0);
        if character == '\n' || (used > 0.0 && used + character_width > width_pixels) {
            lines.push(String::new());
            used = 0.0;
            previous = None;
            if character == '\n' {
                continue;
            }
        }
        if let Some(line) = lines.last_mut() {
            line.push(character);
        }
        used += character_width;
        previous = Some(character);
    }
    Ok(lines)
}

fn scale_cells(value: usize, total: usize, rendered: u16) -> u16 {
    u16::try_from(value.saturating_mul(usize::from(rendered)) / total.max(1))
        .unwrap_or(rendered)
        .min(rendered)
}

fn escape_typst_string(text: &str) -> String {
    text.chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' | '\r' | '\t' => vec![' '],
            control if control.is_control() => vec!['�'],
            other => vec![other],
        })
        .collect()
}

fn cells_for(pixels: u32, cell: u32) -> u16 {
    let cells = pixels.saturating_add(cell.saturating_sub(1)) / cell.max(1);
    u16::try_from(cells).unwrap_or(u16::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceSpan, TableCell, TableRow, TextStyle};

    fn table(text: &str) -> TableElement {
        TableElement {
            alignments: vec![TableAlignment::Left, TableAlignment::Right],
            rows: vec![
                TableRow {
                    header: true,
                    cells: vec![cell("Name", 0..4), cell("Value", 5..10)],
                    source: 0..10,
                },
                TableRow {
                    header: false,
                    cells: vec![cell(text, 11..11 + text.len()), cell("42", 20..22)],
                    source: 11..22,
                },
            ],
            source: 0..22,
            style: TextStyle::default(),
        }
    }

    fn cell(text: &str, source: SourceSpan) -> TableCell {
        TableCell {
            text: text.to_owned(),
            source,
            style: TextStyle::default(),
        }
    }

    #[test]
    fn generated_source_escapes_cell_content_and_has_no_resolvers() {
        let plan = TablePlan::build(
            0,
            &table("#import \"evil.typ\" \\\""),
            40,
            PixelSize::FALLBACK,
        )
        .expect("table should plan");

        assert!(plan.source.contains("#text"));
        assert!(plan.source.contains("#import \\\"evil.typ\\\""));
        assert!(!plan.source.contains("package"));
    }

    #[test]
    fn generated_source_uses_booktabs_style_horizontal_rules() {
        let plan = TablePlan::build(0, &table("text"), 40, PixelSize::FALLBACK)
            .expect("table should plan");

        assert!(plan.source.contains("if y == 0"));
        assert!(plan.source.contains("top: 5pt, bottom: 5pt"));
        assert!(plan.source.contains("else { (x: 5pt, y: 3pt) }"));
        assert!(plan.source.contains("top: 1.4pt"));
        assert!(plan.source.contains("bottom: 0.6pt"));
        assert!(plan.source.contains("else if y == 1"));
        assert!(plan.source.contains("bottom: 1.4pt"));
        assert!(plan.source.contains("else { none }"));
        plan.render(PixelSize::FALLBACK)
            .expect("booktabs-style rules should compile and render");
    }

    #[test]
    fn too_narrow_tables_choose_text_fallback() {
        let error = TablePlan::build(0, &table("text"), 12, PixelSize::FALLBACK)
            .expect_err("two columns need borders and minimum readable widths");
        assert!(error.contains("at least"));
    }

    #[test]
    fn width_and_cell_metrics_change_the_cache_identity() {
        let table = table("text");
        let base = TablePlan::cache_identity(&table, 80, PixelSize::FALLBACK);
        let narrow = TablePlan::cache_identity(&table, 40, PixelSize::FALLBACK);
        let large_cells = TablePlan::cache_identity(
            &table,
            80,
            PixelSize {
                width: 10,
                height: 20,
            },
        );
        assert_ne!(base, narrow);
        assert_ne!(base, large_cells);
    }

    #[test]
    fn short_tables_fit_content_instead_of_filling_the_viewport() {
        let plan = TablePlan::build(0, &table("short"), 80, PixelSize::FALLBACK)
            .expect("short table should plan");

        assert_eq!(plan.columns, 14);
        assert!(plan.columns < 80);
        assert!(plan.source.contains("size: 10pt"));
        assert!(plan.source.contains("fill: none"));
        assert_eq!(plan.rows, 4);
        assert_eq!(plan.key.pixels.height, 0);
    }

    #[test]
    fn rendered_table_keeps_the_terminal_background_transparent() {
        let (_, image) = TablePlan::build(0, &table("short"), 80, PixelSize::FALLBACK)
            .expect("short table should plan")
            .render(PixelSize::FALLBACK)
            .expect("short table should render");
        let alpha = image.pixels().map(|pixel| pixel[3]).collect::<Vec<_>>();

        assert!(alpha.contains(&0));
        assert!(alpha.iter().any(|channel| *channel > 0));
    }

    #[test]
    fn rendered_tables_use_cell_aligned_canvases_at_multiple_cell_sizes() {
        for cell in [
            PixelSize {
                width: 8,
                height: 16,
            },
            PixelSize {
                width: 9,
                height: 17,
            },
        ] {
            let (plan, raster) = TablePlan::build(0, &table("short"), 80, cell)
                .expect("table should plan")
                .render(cell)
                .expect("table should render");

            assert_eq!(raster.width(), u32::from(plan.columns) * cell.width);
            assert_eq!(raster.height(), u32::from(plan.rows) * cell.height);
            assert!(raster.pixels().any(|pixel| pixel.0[3] > 0));
        }
    }

    #[test]
    fn four_logical_table_rows_include_rule_spacing() {
        let mut table = table("short");
        table.rows.extend((0..2).map(|_| TableRow {
            header: false,
            cells: vec![cell("a", 0..1), cell("b", 2..3)],
            source: 0..3,
        }));
        let plan = TablePlan::build(0, &table, 80, PixelSize::FALLBACK)
            .expect("four-row table should plan");

        let (rendered, _) = plan
            .render(PixelSize::FALLBACK)
            .expect("four-row table should render");
        assert_eq!(rendered.rows, 6);
        assert_eq!(rendered.key.pixels.height, 6 * PixelSize::FALLBACK.height);
    }

    #[test]
    fn unicode_wrapping_is_decided_before_typst_layout() {
        let cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let lines = wrap_text("ab螺鈿cd", content_width_pixels(5, cell_pixels))
            .expect("embedded font should load");
        assert!(lines.len() > 1);
        let source = typst_source(&table("ab螺鈿cd"), &[8, 8], 320, cell_pixels)
            .expect("Typst source should build");
        assert!(source.contains("#box"));
        assert!(source.contains("#linebreak()"));
        assert!(source.contains("120.0000pt"));
    }

    #[test]
    fn missing_embedded_glyphs_trigger_text_fallback() {
        let error = TablePlan::build(0, &table("emoji 😀"), 40, PixelSize::FALLBACK)
            .expect_err("missing embedded glyph must reject the raster plan");
        assert!(error.contains("no glyph"));
    }

    #[test]
    fn structural_limits_reject_only_values_above_the_boundary() {
        let mut columns = table("ok");
        columns.alignments = vec![TableAlignment::Default; MAX_TABLE_COLUMNS];
        columns.rows[0].cells = (0..MAX_TABLE_COLUMNS).map(|_| cell("x", 0..1)).collect();
        columns.rows[1].cells = (0..MAX_TABLE_COLUMNS).map(|_| cell("x", 0..1)).collect();
        assert!(validate_table(&columns).is_ok());
        columns.alignments.push(TableAlignment::Default);
        assert!(validate_table(&columns).is_err());

        let mut cells = table("ok");
        cells.alignments = vec![TableAlignment::Default; MAX_TABLE_COLUMNS];
        cells.rows = (0..MAX_TABLE_CELLS / MAX_TABLE_COLUMNS)
            .map(|_| TableRow {
                header: false,
                cells: (0..MAX_TABLE_COLUMNS).map(|_| cell("x", 0..1)).collect(),
                source: 0..1,
            })
            .collect();
        assert!(validate_table(&cells).is_ok());
        cells.rows.push(TableRow {
            header: false,
            cells: vec![cell("x", 0..1)],
            source: 0..1,
        });
        assert!(validate_table(&cells).is_err());
    }

    #[test]
    fn raster_limits_accept_the_boundary_and_reject_the_next_value() {
        let cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let max_height = cell_pixels.height * MAX_TABLE_RASTER_ROWS;
        assert!(validate_raster_bounds(1, max_height, cell_pixels).is_ok());
        assert!(validate_raster_bounds(1, max_height + 1, cell_pixels).is_err());
        let max_width = u32::try_from(MAX_TABLE_RASTER_BYTES / 4).expect("limit fits u32");
        assert!(validate_raster_bounds(max_width, 1, cell_pixels).is_ok());
        assert!(validate_raster_bounds(max_width + 1, 1, cell_pixels).is_err());
    }

    #[test]
    fn typst_does_not_add_wraps_to_a_rust_fitted_line() {
        let cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let short = TablePlan::build(0, &table("x"), 40, cell_pixels)
            .expect("short table should plan")
            .render(cell_pixels)
            .expect("short table should render")
            .0;
        let fitted =
            TablePlan::build(0, &table("word"), 40, cell_pixels).expect("fitted table should plan");
        assert!(!fitted.source.contains("#linebreak()"));
        let fitted = fitted
            .render(cell_pixels)
            .expect("fitted table should render")
            .0;
        assert_eq!(fitted.key.pixels.height, short.key.pixels.height);
    }
}
