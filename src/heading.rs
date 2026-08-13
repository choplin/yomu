use std::fmt;
use std::sync::OnceLock;

use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use fontdue::{Font, FontSettings};
use image::{Rgba, RgbaImage};

use crate::{ElementKey, HeadingElement, PixelSize, RasterAssetKey};

const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSansJP.ttf");
const HEADING_RASTERIZER_VERSION: u16 = 4;
const MAX_RASTER_DIMENSION: u32 = 8_192;
const MAX_RASTER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HEADING_BYTES: usize = 64 * 1024;
const MAX_HEADING_CHARACTERS: usize = 16_384;

static FONT: OnceLock<Result<Font, String>> = OnceLock::new();
static FONT_HASH: OnceLock<[u8; 32]> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PositionedGlyph {
    character: char,
    font_index: usize,
    x: u32,
    baseline: u32,
}

struct LoadedFont {
    font: Font,
    family: String,
}

pub(crate) struct HeadingFonts {
    configured: Vec<LoadedFont>,
    identity: [u8; 32],
}

impl fmt::Debug for HeadingFonts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeadingFonts")
            .field(
                "configured",
                &self
                    .configured
                    .iter()
                    .map(|loaded| &loaded.family)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for HeadingFonts {
    fn default() -> Self {
        Self {
            configured: Vec::new(),
            identity: font_hash(),
        }
    }
}

impl HeadingFonts {
    pub(crate) fn load(families: &[String]) -> Result<Self, String> {
        if families.is_empty() {
            return Ok(Self::default());
        }
        let mut database = Database::new();
        database.load_system_fonts();
        Self::load_from_database(families, &database)
    }

    fn load_from_database(families: &[String], database: &Database) -> Result<Self, String> {
        let mut configured = Vec::with_capacity(families.len());
        let mut hash = blake3::Hasher::new();
        hash.update(b"raden-heading-font-stack-v1");
        for family in families {
            let Some(id) = database.query(&Query {
                families: &[Family::Name(family)],
                weight: Weight::NORMAL,
                stretch: Stretch::Normal,
                style: Style::Normal,
            }) else {
                continue;
            };
            let (bytes, collection_index) = database
                .with_face_data(id, |data, index| (data.to_vec(), index))
                .ok_or_else(|| format!("could not read heading font family {family:?}"))?;
            let font = Font::from_bytes(
                bytes.as_slice(),
                FontSettings {
                    collection_index,
                    ..FontSettings::default()
                },
            )
            .map_err(|error| format!("could not load heading font family {family:?}: {error}"))?;
            hash.update(&(family.len() as u64).to_le_bytes());
            hash.update(family.as_bytes());
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
            configured.push(LoadedFont {
                font,
                family: family.clone(),
            });
        }
        if configured.is_empty() {
            return Err(format!(
                "none of the configured heading font families are installed: {}",
                families
                    .iter()
                    .map(|family| format!("{family:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        hash.update(FONT_BYTES);
        Ok(Self {
            configured,
            identity: *hash.finalize().as_bytes(),
        })
    }

    fn select(&self, character: char) -> Option<(usize, &Font)> {
        self.configured
            .iter()
            .enumerate()
            .find_map(|(index, loaded)| {
                loaded
                    .font
                    .has_glyph(character)
                    .then_some((index, &loaded.font))
            })
            .or_else(|| {
                font()
                    .ok()
                    .filter(|font| font.has_glyph(character))
                    .map(|font| (self.configured.len(), font))
            })
    }

    fn get(&self, index: usize) -> Result<&Font, String> {
        if index == self.configured.len() {
            font()
        } else {
            self.configured
                .get(index)
                .map(|loaded| &loaded.font)
                .ok_or_else(|| format!("heading font index {index} is unavailable"))
        }
    }
}

#[derive(Debug)]
pub(crate) struct HeadingPlan {
    pub key: RasterAssetKey,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
    glyphs: Vec<PositionedGlyph>,
    font_pixels: f32,
    color: [u8; 3],
    stroke_radius: u32,
    underline: (u32, u32, u32, u32),
}

impl HeadingPlan {
    pub(crate) fn cache_identity_with_fonts(
        heading: &HeadingElement,
        available_columns: u16,
        cell_pixels: PixelSize,
        fonts: &HeadingFonts,
    ) -> Result<[u8; 32], String> {
        validate_heading_length(heading)?;
        let width = raster_width(available_columns, cell_pixels);
        let mut hash = blake3::Hasher::new();
        hash.update(&fonts.identity);
        hash.update(b"heading-dark-v1");
        hash.update(&[heading.level]);
        hash.update(heading.text.as_bytes());
        hash.update(&width.to_le_bytes());
        hash.update(&cell_pixels.width.to_le_bytes());
        hash.update(&cell_pixels.height.to_le_bytes());
        hash.update(&HEADING_RASTERIZER_VERSION.to_le_bytes());
        Ok(*hash.finalize().as_bytes())
    }

    pub(crate) fn element_key(index: usize) -> ElementKey {
        ElementKey((1_u64 << 63) | (index as u64 + 1))
    }

    #[cfg(test)]
    fn build(
        index: usize,
        heading: &HeadingElement,
        available_columns: u16,
        cell_pixels: PixelSize,
    ) -> Result<Self, String> {
        Self::build_with_fonts(
            index,
            heading,
            available_columns,
            cell_pixels,
            &HeadingFonts::default(),
        )
    }

    pub(crate) fn build_with_fonts(
        index: usize,
        heading: &HeadingElement,
        available_columns: u16,
        cell_pixels: PixelSize,
        fonts: &HeadingFonts,
    ) -> Result<Self, String> {
        let identity =
            Self::cache_identity_with_fonts(heading, available_columns, cell_pixels, fonts)?;
        let character_count = heading.text.chars().count();
        let width = raster_width(available_columns, cell_pixels);
        let scale = if heading.level == 1 { 2.0 } else { 1.5 };
        let stroke_radius = if heading.level == 1 { 2 } else { 1 };
        let font_pixels = (cell_pixels.height.max(1) as f32 * scale).max(16.0);
        let horizontal_padding = if width > cell_pixels.width.max(1).saturating_mul(2) {
            cell_pixels.width.max(1)
        } else {
            0
        };
        let vertical_padding = if heading.level == 1 {
            cell_pixels.height.max(1) / 2
        } else {
            cell_pixels.height.max(1) / 4
        };
        let line_height = (font_pixels * 1.25).ceil() as u32;
        let right = width
            .saturating_sub(horizontal_padding)
            .max(horizontal_padding + 1);
        let mut x = horizontal_padding;
        let mut baseline = vertical_padding.saturating_add(font_pixels.ceil() as u32);
        let mut glyphs = Vec::with_capacity(character_count.min(256));
        let mut lines = 1u32;
        let mut max_line_x = horizontal_padding;

        for character in heading.text.chars() {
            if character == '\n' {
                x = horizontal_padding;
                baseline = baseline.saturating_add(line_height);
                lines = lines.saturating_add(1);
                continue;
            }
            let Some((font_index, font)) = fonts.select(character) else {
                return Err(format!(
                    "heading font stack does not contain U+{:04X}",
                    u32::from(character)
                ));
            };
            let metrics = font.metrics(character, font_pixels);
            if !character.is_whitespace() && metrics.advance_width <= 0.0 {
                return Err(format!(
                    "heading character U+{:04X} requires text shaping",
                    u32::from(character)
                ));
            }
            let advance = metrics.advance_width.ceil().max(1.0) as u32;
            let glyph_left = i64::from(horizontal_padding) + metrics.xmin as i64;
            let glyph_right = glyph_left
                .saturating_add(metrics.width as i64)
                .saturating_add(i64::from(stroke_radius));
            if glyph_left < 0
                || glyph_right > i64::from(width)
                || advance > right.saturating_sub(horizontal_padding)
            {
                return Err(format!(
                    "heading glyph U+{:04X} does not fit in {width} pixels",
                    u32::from(character),
                ));
            }
            if x > horizontal_padding && x.saturating_add(advance) > right {
                x = horizontal_padding;
                baseline = baseline.saturating_add(line_height);
                lines = lines.saturating_add(1);
            }
            if !(character.is_whitespace() && x == horizontal_padding) {
                glyphs.push(PositionedGlyph {
                    character,
                    font_index,
                    x,
                    baseline,
                });
                x = x.saturating_add(advance);
                max_line_x = max_line_x.max(x);
            }
            let projected_height = vertical_padding
                .saturating_mul(2)
                .saturating_add(lines.saturating_mul(line_height));
            if projected_height > MAX_RASTER_DIMENSION {
                return Err(format!(
                    "heading raster height exceeds {MAX_RASTER_DIMENSION} pixels"
                ));
            }
        }

        let content_height = vertical_padding
            .saturating_mul(2)
            .saturating_add(lines.saturating_mul(line_height));
        let height = align_to_cell(content_height, cell_pixels.height);
        if height > MAX_RASTER_DIMENSION {
            return Err(format!(
                "heading raster is {width}x{height}; maximum dimension is {MAX_RASTER_DIMENSION} pixels"
            ));
        }
        let bytes = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        if height == 0 || bytes > MAX_RASTER_BYTES {
            return Err(format!(
                "heading raster is {width}x{height} ({bytes} bytes); limit is {MAX_RASTER_BYTES} bytes"
            ));
        }

        let pixels = PixelSize { width, height };
        let underline_start = horizontal_padding;
        let underline_end = max_line_x
            .saturating_add(horizontal_padding)
            .min(right)
            .max(underline_start.saturating_add(1));
        let underline_thickness = if heading.level == 1 { 2 } else { 1 };
        let underline_y = content_height
            .saturating_sub(vertical_padding.max(underline_thickness))
            .saturating_sub(underline_thickness);
        let key = RasterAssetKey {
            content_hash: identity,
            pixels,
            rasterizer_version: HEADING_RASTERIZER_VERSION,
        };

        Ok(Self {
            key,
            element_key: Self::element_key(index),
            columns: cells_for(width, cell_pixels.width),
            rows: cells_for(height, cell_pixels.height),
            glyphs,
            font_pixels,
            color: if heading.level == 1 {
                [70, 210, 225]
            } else {
                [105, 155, 245]
            },
            stroke_radius,
            underline: (
                underline_start,
                underline_end,
                underline_y,
                underline_thickness,
            ),
        })
    }

    #[cfg(test)]
    fn render(&self) -> Result<RgbaImage, String> {
        self.render_with_fonts(&HeadingFonts::default())
    }

    pub(crate) fn render_with_fonts(&self, fonts: &HeadingFonts) -> Result<RgbaImage, String> {
        let mut canvas = RgbaImage::new(self.key.pixels.width, self.key.pixels.height);
        for glyph in &self.glyphs {
            let font = fonts.get(glyph.font_index)?;
            let (metrics, coverage) = font.rasterize(glyph.character, self.font_pixels);
            let left = i64::from(glyph.x) + metrics.xmin as i64;
            let top = i64::from(glyph.baseline) - metrics.height as i64 - metrics.ymin as i64;
            for y_offset in 0..=self.stroke_radius {
                for x_offset in 0..=self.stroke_radius {
                    draw_coverage(
                        &mut canvas,
                        &metrics,
                        &coverage,
                        left + i64::from(x_offset),
                        top + i64::from(y_offset),
                        self.color,
                    );
                }
            }
        }
        draw_underline(&mut canvas, self.underline, self.color);
        Ok(canvas)
    }
}

fn draw_underline(canvas: &mut RgbaImage, line: (u32, u32, u32, u32), color: [u8; 3]) {
    let (start, end, y, thickness) = line;
    for row in y..y.saturating_add(thickness).min(canvas.height()) {
        for column in start..end.min(canvas.width()) {
            *canvas.get_pixel_mut(column, row) = Rgba([color[0], color[1], color[2], 190]);
        }
    }
}

fn validate_heading_length(heading: &HeadingElement) -> Result<(), String> {
    if heading.text.len() > MAX_HEADING_BYTES {
        return Err(format!(
            "heading contains {} UTF-8 bytes; limit is {MAX_HEADING_BYTES}",
            heading.text.len()
        ));
    }
    let character_count = heading.text.chars().count();
    if character_count > MAX_HEADING_CHARACTERS {
        return Err(format!(
            "heading contains {character_count} characters; limit is {MAX_HEADING_CHARACTERS}"
        ));
    }
    Ok(())
}

fn raster_width(available_columns: u16, cell_pixels: PixelSize) -> u32 {
    u32::from(available_columns.max(1))
        .saturating_mul(cell_pixels.width.max(1))
        .min(MAX_RASTER_DIMENSION)
}

fn align_to_cell(pixels: u32, cell_pixels: u32) -> u32 {
    let cell_pixels = cell_pixels.max(1);
    pixels.div_ceil(cell_pixels).saturating_mul(cell_pixels)
}

fn font() -> Result<&'static Font, String> {
    FONT.get_or_init(|| {
        Font::from_bytes(FONT_BYTES, FontSettings::default())
            .map_err(|error| format!("could not load embedded Noto Sans JP: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn font_hash() -> [u8; 32] {
    *FONT_HASH.get_or_init(|| *blake3::hash(FONT_BYTES).as_bytes())
}

fn draw_coverage(
    canvas: &mut RgbaImage,
    metrics: &fontdue::Metrics,
    coverage: &[u8],
    left: i64,
    top: i64,
    color: [u8; 3],
) {
    for row in 0..metrics.height {
        for column in 0..metrics.width {
            let Some(alpha) =
                coverage.get(row.saturating_mul(metrics.width).saturating_add(column))
            else {
                continue;
            };
            if *alpha == 0 {
                continue;
            }
            let x = left + column as i64;
            let y = top + row as i64;
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                continue;
            };
            let Some(pixel) = in_bounds_pixel(canvas, x, y) else {
                continue;
            };
            let blended_alpha = pixel.0[3].max(*alpha);
            *pixel = Rgba([color[0], color[1], color[2], blended_alpha]);
        }
    }
}

fn in_bounds_pixel(image: &mut RgbaImage, x: u32, y: u32) -> Option<&mut Rgba<u8>> {
    (x < image.width() && y < image.height()).then(|| image.get_pixel_mut(x, y))
}

fn cells_for(pixels: u32, cell: u32) -> u16 {
    let cells = pixels.saturating_add(cell.saturating_sub(1)) / cell.max(1);
    u16::try_from(cells).unwrap_or(u16::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextStyle;

    fn heading(level: u8, text: &str) -> HeadingElement {
        HeadingElement {
            level,
            text: text.to_owned(),
            source: 0..text.len(),
            style: TextStyle {
                heading: Some(level),
                ..TextStyle::default()
            },
        }
    }

    #[test]
    fn levels_have_distinct_size_color_and_cache_keys() {
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let h1 = HeadingPlan::build(0, &heading(1, "Heading"), 40, cell).expect("H1 plans");
        let h2 = HeadingPlan::build(0, &heading(2, "Heading"), 40, cell).expect("H2 plans");

        assert!(h1.rows > h2.rows);
        assert_ne!(h1.color, h2.color);
        assert_ne!(h1.key, h2.key);
    }

    #[test]
    fn raster_height_is_cell_aligned_without_scaling_the_content() {
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        for level in [1, 2] {
            let plan = HeadingPlan::build(0, &heading(level, "Heading"), 40, cell)
                .expect("heading should plan");
            let raster = plan.render().expect("heading should render");

            assert_eq!(plan.key.pixels.height, u32::from(plan.rows) * cell.height);
            assert!(
                raster
                    .rows()
                    .next_back()
                    .expect("raster has a final row")
                    .all(|pixel| pixel.0[3] == 0),
                "cell alignment should add transparent padding below H{level}"
            );
        }
    }

    #[test]
    fn unicode_and_long_headings_wrap_without_losing_glyphs() {
        let text = "漆と螺鈿の長い見出し heading";
        let plan = HeadingPlan::build(
            0,
            &heading(1, text),
            8,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect("Unicode heading should plan");
        let raster = plan.render().expect("Unicode heading should rasterize");

        assert_eq!(
            plan.glyphs.len(),
            text.chars().filter(|c| *c != '\n').count()
        );
        assert!(plan.rows > 2);
        assert!(raster.pixels().any(|pixel| pixel.0[3] > 0));
    }

    #[test]
    fn one_column_heading_falls_back_instead_of_clipping() {
        let error = HeadingPlan::build(
            0,
            &heading(2, "漆"),
            1,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect_err("one-column heading should not clip a wide glyph");

        assert!(error.contains("does not fit"));
    }

    #[test]
    fn unsupported_unicode_falls_back_instead_of_using_missing_glyphs() {
        let error = HeadingPlan::build(
            0,
            &heading(1, "Arabic ش and emoji 🧪"),
            80,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect_err("unsupported Unicode should not rasterize as missing glyphs");

        assert!(error.contains("does not contain U+"));
    }

    #[test]
    fn glyph_bearings_and_synthetic_bold_must_fit_the_canvas() {
        let cell = PixelSize {
            width: 8,
            height: 16,
        };
        let negative_bearing = HeadingPlan::build(0, &heading(1, "j"), 1, cell)
            .expect_err("negative bearing must not clip at the left edge");
        let bold_overhang = HeadingPlan::build(0, &heading(1, "Y"), 2, cell)
            .expect_err("synthetic bold must not clip at the right edge");

        assert!(negative_bearing.contains("does not fit"));
        assert!(bold_overhang.contains("does not fit"));
    }

    #[test]
    fn combining_marks_fall_back_without_a_shaping_engine() {
        let error = HeadingPlan::build(
            0,
            &heading(2, "Cafe\u{301}"),
            80,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect_err("combining marks require shaping");

        assert!(error.contains("requires text shaping"));
    }

    #[test]
    fn width_and_cell_metrics_are_part_of_the_cache_key() {
        let heading = heading(1, "Cache identity");
        let base = HeadingPlan::build(
            0,
            &heading,
            40,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect("base heading should plan");
        let narrower = HeadingPlan::build(
            0,
            &heading,
            20,
            PixelSize {
                width: 8,
                height: 16,
            },
        )
        .expect("narrow heading should plan");
        let larger_cells = HeadingPlan::build(
            0,
            &heading,
            40,
            PixelSize {
                width: 10,
                height: 20,
            },
        )
        .expect("large-cell heading should plan");

        assert_ne!(base.key, narrower.key);
        assert_ne!(base.key, larger_cells.key);
    }

    #[test]
    fn configured_fonts_take_precedence_over_the_embedded_fallback() {
        let mut database = Database::new();
        database.load_font_data(FONT_BYTES.to_vec());
        let fonts = HeadingFonts::load_from_database(&["Noto Sans JP".to_owned()], &database)
            .expect("font family should resolve");
        let plan = HeadingPlan::build_with_fonts(
            0,
            &heading(1, "Configured 漆"),
            40,
            PixelSize {
                width: 8,
                height: 16,
            },
            &fonts,
        )
        .expect("configured font should rasterize the heading");

        assert!(plan.glyphs.iter().all(|glyph| glyph.font_index == 0));
        assert!(
            plan.render_with_fonts(&fonts)
                .expect("configured font should render")
                .pixels()
                .any(|pixel| pixel.0[3] > 0)
        );
    }

    #[test]
    fn unavailable_fallback_families_are_skipped() {
        let mut database = Database::new();
        database.load_font_data(FONT_BYTES.to_vec());
        let fonts = HeadingFonts::load_from_database(
            &["Missing Family".to_owned(), "Noto Sans JP".to_owned()],
            &database,
        )
        .expect("an available fallback should resolve the stack");

        assert_eq!(fonts.configured.len(), 1);
        assert_eq!(fonts.configured[0].family, "Noto Sans JP");
        assert!(
            HeadingFonts::load_from_database(&["Missing Family".to_owned()], &database)
                .expect_err("an entirely unavailable stack should fail")
                .contains("none of the configured")
        );
    }
}
