use image::RgbaImage;
use std::sync::OnceLock;
use typst::layout::Abs;
use typst_as_lib::TypstEngine;

use crate::{ElementKey, MathElement, MathForm, PixelSize, RasterAssetKey};

const INLINE_ROWS: u16 = 1;
const DISPLAY_ROWS: u16 = 4;
const INLINE_VERTICAL_GUTTER: u32 = 1;
const MAX_EXPRESSION_BYTES: usize = 16 * 1024;
const MAX_RASTER_BYTES: u64 = 64 * 1024 * 1024;
const MATH_RASTERIZER_VERSION: u16 = 6;
const FOREGROUND: &str = "#d8dee9";
static FONT_IDENTITY: OnceLock<[u8; 32]> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct MathPlan {
    pub key: RasterAssetKey,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
    form: MathForm,
    source: String,
    available_pixels: u32,
}

impl MathPlan {
    pub(crate) fn build(
        index: usize,
        math: &MathElement,
        available_columns: u16,
        available_rows: u16,
        cell_pixels: PixelSize,
    ) -> Result<Self, String> {
        if math.expression.trim().is_empty() {
            return Err("math expression is empty".to_owned());
        }
        if math.expression.len() > MAX_EXPRESSION_BYTES {
            return Err(format!(
                "math expression is {} bytes; limit is {MAX_EXPRESSION_BYTES}",
                math.expression.len()
            ));
        }
        let converted = tex2typst_rs::tex2typst(&math.expression)
            .map_err(|error| format!("TeX conversion failed: {error}"))?;
        let font_points = match math.form {
            MathForm::Inline => 11,
            MathForm::Display => 16,
        };
        let source = format!(
            "#set page(width: auto, height: auto, margin: 1pt, fill: none)\n#set text(size: {font_points}pt, fill: rgb(\"{FOREGROUND}\"))\n${converted}$\n"
        );
        let rows = effective_rows(math.form, available_rows);
        let available_pixels =
            u32::from(available_columns.max(1)).saturating_mul(cell_pixels.width.max(1));
        let identity = Self::cache_identity(math, available_columns, available_rows, cell_pixels);
        Ok(Self {
            key: RasterAssetKey {
                content_hash: identity,
                pixels: PixelSize {
                    width: available_pixels,
                    height: u32::from(rows).saturating_mul(cell_pixels.height.max(1)),
                },
                rasterizer_version: MATH_RASTERIZER_VERSION,
            },
            element_key: Self::element_key(index),
            columns: available_columns.max(1),
            rows,
            form: math.form,
            source,
            available_pixels,
        })
    }

    pub(crate) fn cache_identity(
        math: &MathElement,
        available_columns: u16,
        available_rows: u16,
        cell_pixels: PixelSize,
    ) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tex2typst-rs-0.4.1");
        hasher.update(b"typst-0.15");
        hasher.update(b"resvg-0.47");
        hasher.update(&MATH_RASTERIZER_VERSION.to_le_bytes());
        hasher.update(&[match math.form {
            MathForm::Inline => 0,
            MathForm::Display => 1,
        }]);
        hasher.update(&available_columns.to_le_bytes());
        hasher.update(&effective_rows(math.form, available_rows).to_le_bytes());
        hasher.update(&cell_pixels.width.to_le_bytes());
        hasher.update(&cell_pixels.height.to_le_bytes());
        hasher.update(FOREGROUND.as_bytes());
        hasher.update(math.expression.as_bytes());
        hasher.update(font_identity());
        *hasher.finalize().as_bytes()
    }

    pub(crate) fn element_key(index: usize) -> ElementKey {
        ElementKey((1_u64 << 61) | (index as u64 + 1))
    }

    pub(crate) fn render(mut self, cell_pixels: PixelSize) -> Result<(Self, RgbaImage), String> {
        let engine = TypstEngine::builder()
            .main_file(self.source.clone())
            .fonts(typst_assets::fonts())
            .build();
        let warned = engine.compile();
        if !warned.warnings.is_empty() {
            return Err(format!(
                "Typst math compilation produced warnings: {}",
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
            .map_err(|error| format!("Typst math compilation failed: {error}"))?;
        let svg = typst_svg::svg_merged(&document, &Default::default(), Abs::zero());
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
            .map_err(|error| format!("Typst math SVG could not be parsed: {error}"))?;
        // Typst's page size is not a reliable content boundary: glyph bearings
        // and math accents can extend outside it. Fit and translate the actual
        // painted bounds so no visible pixels are lost at the canvas edges.
        let bounds = tree.root().abs_layer_bounding_box();
        let cell_width = cell_pixels.width.max(1);
        let cell_height = cell_pixels.height.max(1);
        let desired_height = u32::from(self.rows).saturating_mul(cell_height);
        let preferred_gutter = match self.form {
            MathForm::Inline => INLINE_VERTICAL_GUTTER,
            // A four-row canvas with half a cell above and below leaves three
            // rows for the formula without inserting full blank lines.
            MathForm::Display => cell_height / 2,
        };
        let vertical_gutter = preferred_gutter.min(desired_height.saturating_sub(1) / 2);
        let content_height = desired_height
            .saturating_sub(vertical_gutter.saturating_mul(2))
            .saturating_sub(1)
            .max(1) as f32;
        let height_scale = content_height / bounds.height().max(1.0);
        let width_scale = self.available_pixels as f32 / bounds.width().max(1.0);
        let scale = height_scale.min(width_scale);
        let rendered_width =
            ((bounds.width() * scale).ceil().max(1.0) as u32).min(self.available_pixels);
        let rendered_height = (bounds.height() * scale).ceil().max(1.0) as u32;
        self.columns = cells_for(rendered_width, cell_width);
        let width = u32::from(self.columns).saturating_mul(cell_width);
        let height = desired_height;
        validate_raster(width, height)?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| "math raster has invalid dimensions".to_owned())?;
        let x = width.saturating_sub(rendered_width) as f32 / 2.0;
        let y = height.saturating_sub(rendered_height) as f32 / 2.0;
        let translation_x = x - bounds.x() * scale;
        let translation_y = y - bounds.y() * scale;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_row(
                scale,
                0.0,
                0.0,
                scale,
                translation_x,
                translation_y,
            ),
            &mut pixmap.as_mut(),
        );
        self.key.pixels = PixelSize { width, height };
        Ok((
            self,
            RgbaImage::from_raw(pixmap.width(), pixmap.height(), pixmap.take_demultiplied())
                .ok_or_else(|| "math raster byte length did not match its dimensions".to_owned())?,
        ))
    }
}

fn font_identity() -> &'static [u8; 32] {
    FONT_IDENTITY.get_or_init(|| {
        let mut hasher = blake3::Hasher::new();
        for font in typst_assets::fonts() {
            hasher.update(blake3::hash(font).as_bytes());
        }
        *hasher.finalize().as_bytes()
    })
}

fn effective_rows(form: MathForm, available_rows: u16) -> u16 {
    match form {
        MathForm::Inline => INLINE_ROWS,
        MathForm::Display => DISPLAY_ROWS,
    }
    .min(available_rows.max(1))
}

fn validate_raster(width: u32, height: u32) -> Result<(), String> {
    let bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if width == 0 || height == 0 || bytes > MAX_RASTER_BYTES {
        return Err(format!(
            "math raster is {width}x{height} ({bytes} bytes); limit is {MAX_RASTER_BYTES} bytes"
        ));
    }
    Ok(())
}

fn cells_for(pixels: u32, cell: u32) -> u16 {
    let cells = pixels.saturating_add(cell.saturating_sub(1)) / cell.max(1);
    u16::try_from(cells).unwrap_or(u16::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceSpan, TextStyle};

    fn math(expression: &str, form: MathForm) -> MathElement {
        MathElement {
            expression: expression.to_owned(),
            form,
            source: SourceSpan::default(),
            style: TextStyle::default(),
        }
    }

    fn visible_vertical_bounds(raster: &RgbaImage) -> (u32, u32) {
        let occupied_rows = raster
            .enumerate_pixels()
            .filter_map(|(_, y, pixel)| (pixel[3] != 0).then_some(y))
            .collect::<Vec<_>>();
        let first = occupied_rows
            .iter()
            .min()
            .copied()
            .expect("math raster should contain visible pixels");
        let last = occupied_rows
            .iter()
            .max()
            .copied()
            .expect("math raster should contain visible pixels");

        (first, last)
    }

    fn assert_vertical_transparency(raster: &RgbaImage) {
        let (first, last) = visible_vertical_bounds(raster);
        assert!(first > 0, "visible pixels touched the top edge");
        assert!(
            last < raster.height() - 1,
            "visible pixels touched the bottom edge"
        );
    }

    #[test]
    fn cache_identity_tracks_form_size_and_formula() {
        let pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let inline = math(r"x^2", MathForm::Inline);
        let display = math(r"x^2", MathForm::Display);

        assert_ne!(
            MathPlan::cache_identity(&inline, 80, 20, pixels),
            MathPlan::cache_identity(&display, 80, 20, pixels)
        );
        assert_ne!(
            MathPlan::cache_identity(&inline, 80, 20, pixels),
            MathPlan::cache_identity(&inline, 40, 20, pixels)
        );
        assert_ne!(
            MathPlan::cache_identity(&inline, 80, 20, pixels),
            MathPlan::cache_identity(&math(r"y^2", MathForm::Inline), 80, 20, pixels)
        );
    }

    #[test]
    fn viewport_height_above_the_form_limit_reuses_the_same_identity() {
        let formula = math(r"x^2", MathForm::Inline);
        let pixels = PixelSize::FALLBACK;

        let first = MathPlan::build(0, &formula, 80, 20, pixels)
            .expect("formula should build")
            .key
            .content_hash;
        let taller = MathPlan::build(0, &formula, 80, 40, pixels)
            .expect("formula should build")
            .key
            .content_hash;

        assert_eq!(first, taller);
    }

    #[test]
    fn documented_supported_subset_rasterizes_with_embedded_fonts() {
        let fixtures = [
            (r"x^2 + y_1 = z", MathForm::Inline),
            (r"\alpha + \beta \leq \sqrt{x}", MathForm::Inline),
            (r"\int_0^1 x^2\,dx = \frac{1}{3}", MathForm::Display),
            (
                r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2} \longrightarrow n \in \mathbb{N}",
                MathForm::Display,
            ),
        ];
        let pixels = PixelSize {
            width: 8,
            height: 16,
        };

        for (index, (expression, form)) in fixtures.into_iter().enumerate() {
            let plan = MathPlan::build(index, &math(expression, form), 80, 20, pixels)
                .expect("documented TeX fixture should convert");
            let (plan, raster) = plan
                .render(pixels)
                .expect("documented Typst math fixture should rasterize");

            assert!(plan.columns > 0);
            assert!(plan.rows > 0);
            assert_eq!(
                raster.as_raw().len(),
                (raster.width() * raster.height() * 4) as usize
            );
        }
    }

    #[test]
    fn inline_matches_one_terminal_row_and_display_is_bounded() {
        let pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let inline = MathPlan::build(0, &math(r"x^2", MathForm::Inline), 80, 20, pixels)
            .expect("inline formula should build")
            .render(pixels)
            .expect("inline formula should render")
            .0;
        let display = MathPlan::build(1, &math(r"x^2", MathForm::Display), 80, 20, pixels)
            .expect("display formula should build")
            .render(pixels)
            .expect("display formula should render")
            .0;

        assert_eq!(inline.rows, 1);
        assert_eq!(inline.key.pixels.height, pixels.height);
        assert_eq!(
            inline.key.pixels.width,
            u32::from(inline.columns) * pixels.width
        );
        assert_eq!(display.rows, DISPLAY_ROWS);
        assert_eq!(
            display.key.pixels.height,
            pixels.height * u32::from(DISPLAY_ROWS)
        );
        assert_eq!(
            display.key.pixels.width,
            u32::from(display.columns) * pixels.width
        );
        assert!(display.key.pixels.height > inline.key.pixels.height);
    }

    #[test]
    fn tall_glyphs_keep_transparent_pixels_above_and_below() {
        let pixels = PixelSize {
            width: 8,
            height: 16,
        };
        for (index, (expression, form)) in [
            (r"\sqrt{x_1^2}", MathForm::Inline),
            (r"\int_0^1 \frac{x^2}{1+x}\,dx", MathForm::Display),
        ]
        .into_iter()
        .enumerate()
        {
            let (_, raster) = MathPlan::build(index, &math(expression, form), 80, 20, pixels)
                .expect("formula should build")
                .render(pixels)
                .expect("formula should render");

            assert_vertical_transparency(&raster);
            if form == MathForm::Display {
                let (first, last) = visible_vertical_bounds(&raster);
                let minimum_margin = pixels.height / 2 - 1;
                assert!(first >= minimum_margin);
                assert!(raster.height() - 1 - last >= minimum_margin);
            }
        }
    }

    #[test]
    fn antialiased_edges_use_straight_rgba_for_terminal_compositing() {
        let pixels = PixelSize {
            width: 18,
            height: 42,
        };
        let (_, raster) = MathPlan::build(
            0,
            &math(r"\alpha + \sqrt{x_1^2}", MathForm::Inline),
            80,
            20,
            pixels,
        )
        .expect("formula should build")
        .render(pixels)
        .expect("formula should render");
        let edges = raster
            .pixels()
            .filter(|pixel| (96..=192).contains(&pixel[3]))
            .collect::<Vec<_>>();

        assert!(
            !edges.is_empty(),
            "fixture should contain antialiased edges"
        );
        for pixel in edges {
            assert!(pixel[0].abs_diff(0xd8) <= 3);
            assert!(pixel[1].abs_diff(0xde) <= 3);
            assert!(pixel[2].abs_diff(0xe9) <= 3);
        }
    }

    #[test]
    fn invalid_tex_returns_a_conversion_error() {
        let error = MathPlan::build(
            0,
            &math(r"\frac{", MathForm::Inline),
            80,
            20,
            PixelSize::FALLBACK,
        )
        .expect_err("invalid TeX must fail");

        assert!(error.contains("TeX conversion failed"));
    }
}
