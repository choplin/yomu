use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage};
use percent_encoding::percent_decode_str;

use crate::theme::{RasterizedHeading, RasterizedMath, RasterizedTable};
use crate::{Document, ImageElement, MathElement, SourceSpan, TableElement, TextStyle};
use crate::{
    HeadingElement,
    heading::{HeadingFonts, HeadingPlan},
    math::MathPlan,
    table::TablePlan,
};

const MAX_ENCODED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8_192;
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_CACHE_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_IMAGE_ROWS: u16 = 20;
const RASTERIZER_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ElementKey(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

impl PixelSize {
    pub const FALLBACK: Self = Self {
        width: 8,
        height: 16,
    };
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RasterAssetKey {
    pub content_hash: [u8; 32],
    pub pixels: PixelSize,
    pub rasterizer_version: u16,
}

#[derive(Clone, Debug)]
pub struct RasterAsset {
    pub key: RasterAssetKey,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub(crate) struct HeadingRaster {
    pub asset: Arc<RasterAsset>,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct TableRaster {
    pub asset: Arc<RasterAsset>,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct MathRaster {
    pub asset: Arc<RasterAsset>,
    pub element_key: ElementKey,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedImage {
    pub element_key: ElementKey,
    pub source: SourceSpan,
    label: String,
    pub text_projection: String,
    pub style: TextStyle,
    recipe: Option<RasterRecipe>,
}

impl PreparedImage {
    #[must_use]
    pub fn can_render(&self) -> bool {
        self.recipe.is_some()
    }

    #[must_use]
    pub fn failure_projection(&self, error: &str) -> String {
        format!("unavailable: {} — {error}", self.label)
    }
}

#[derive(Clone, Debug)]
struct RasterSource {
    rgba: RgbaImage,
}

#[derive(Clone, Debug)]
struct RasterRecipe {
    path: PathBuf,
    content_hash: [u8; 32],
    natural_size: PixelSize,
}

#[derive(Clone, Debug)]
pub(crate) struct ImageCatalog {
    images: Vec<PreparedImage>,
    diagnostics: Vec<String>,
}

impl ImageCatalog {
    pub fn load(document: &Document, markdown_path: &Path) -> Self {
        let root = resolve_markdown_root(markdown_path);
        let mut diagnostics = Vec::new();
        let images = document
            .images()
            .iter()
            .enumerate()
            .map(|(index, element)| match &root {
                Ok(root) => prepare_image(index, element, root, &mut diagnostics),
                Err(error) => unavailable(index, element, error.clone(), &mut diagnostics),
            })
            .collect();
        Self {
            images,
            diagnostics,
        }
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&PreparedImage> {
        self.images.get(index)
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

fn resolve_markdown_root(markdown_path: &Path) -> Result<PathBuf, String> {
    let parent = markdown_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_err(|error| {
        format!(
            "could not resolve Markdown directory {}: {error}",
            parent.display()
        )
    })
}

fn prepare_image(
    index: usize,
    element: &ImageElement,
    root: &Path,
    diagnostics: &mut Vec<String>,
) -> PreparedImage {
    let path = match resolve_local_path(root, &element.destination) {
        Ok(path) => path,
        Err(error) => return unavailable(index, element, error, diagnostics),
    };
    match inspect_image(path) {
        Ok(recipe) => PreparedImage {
            element_key: ElementKey(index as u64 + 1),
            source: element.source.clone(),
            label: image_label(element).to_owned(),
            text_projection: fallback_text(element, None),
            style: element.style,
            recipe: Some(recipe),
        },
        Err(error) => unavailable(index, element, error, diagnostics),
    }
}

fn unavailable(
    index: usize,
    element: &ImageElement,
    error: String,
    diagnostics: &mut Vec<String>,
) -> PreparedImage {
    let label = image_label(element);
    diagnostics.push(format!("image {label}: {error}"));
    PreparedImage {
        element_key: ElementKey(index as u64 + 1),
        source: element.source.clone(),
        label: label.to_owned(),
        text_projection: fallback_text(element, Some(&error)),
        style: element.style,
        recipe: None,
    }
}

fn image_label(element: &ImageElement) -> &str {
    if element.alt.trim().is_empty() {
        element.destination.as_str()
    } else {
        element.alt.trim()
    }
}

fn fallback_text(element: &ImageElement, error: Option<&str>) -> String {
    let label = image_label(element);
    match error {
        Some(error) => format!("unavailable: {label} — {error}"),
        None => label.to_owned(),
    }
}

fn resolve_local_path(root: &Path, destination: &str) -> Result<PathBuf, String> {
    if destination.contains("://")
        || destination.starts_with("data:")
        || destination.starts_with("file:")
        || destination.starts_with("//")
    {
        return Err("remote and URL image sources are not supported".to_owned());
    }
    let decoded = percent_decode_str(destination)
        .decode_utf8()
        .map_err(|_| "image path is not valid UTF-8 after percent decoding".to_owned())?;
    let relative = Path::new(decoded.as_ref());
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("image path escapes the Markdown directory".to_owned());
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute image paths are not supported".to_owned());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("image path is empty".to_owned());
    }

    let candidate = root.join(normalized);
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("could not open {}: {error}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err("image path escapes the Markdown directory".to_owned());
    }
    if !canonical.is_file() {
        return Err(format!("{} is not a regular file", canonical.display()));
    }
    Ok(canonical)
}

fn inspect_image(path: PathBuf) -> Result<RasterRecipe, String> {
    let bytes = read_image_bytes(&path)?;
    let content_hash = *blake3::hash(&bytes).as_bytes();
    let mut reader = supported_reader(bytes)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("could not read image dimensions: {error}"))?;
    validate_size(width, height, MAX_DECODED_BYTES)?;
    Ok(RasterRecipe {
        path,
        content_hash,
        natural_size: PixelSize { width, height },
    })
}

fn decode_image(path: &Path, decoded_byte_limit: u64) -> Result<RasterSource, String> {
    let bytes = read_image_bytes(path)?;
    let mut reader = supported_reader(bytes)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(decoded_byte_limit);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("could not decode image: {error}"))?;
    validate_dimensions(&decoded, decoded_byte_limit)?;
    Ok(RasterSource {
        rgba: decoded.into_rgba8(),
    })
}

fn read_image_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_ENCODED_BYTES {
        return Err(format!(
            "encoded file is {} bytes; limit is {MAX_ENCODED_BYTES}",
            metadata.len()
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len().min(MAX_ENCODED_BYTES)).unwrap_or_default(),
    );
    file.take(MAX_ENCODED_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_ENCODED_BYTES {
        return Err(format!(
            "encoded file exceeds the {MAX_ENCODED_BYTES}-byte limit"
        ));
    }
    Ok(bytes)
}

fn supported_reader(bytes: Vec<u8>) -> Result<ImageReader<Cursor<Vec<u8>>>, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("could not identify image format: {error}"))?;
    let Some(format) = reader.format() else {
        return Err("unsupported or unrecognized image format".to_owned());
    };
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(format!("unsupported image format: {format:?}"));
    }
    Ok(reader)
}

fn validate_dimensions(image: &DynamicImage, decoded_byte_limit: u64) -> Result<(), String> {
    validate_size(image.width(), image.height(), decoded_byte_limit)
}

fn validate_size(width: u32, height: u32, decoded_byte_limit: u64) -> Result<(), String> {
    let decoded_bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if width == 0 || height == 0 {
        return Err("image has an empty dimension".to_owned());
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION || decoded_bytes > decoded_byte_limit {
        return Err(format!(
            "decoded image is {width}x{height} ({decoded_bytes} bytes); limits are {MAX_DIMENSION}x{MAX_DIMENSION} and {decoded_byte_limit} bytes"
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct CacheEntry {
    asset: Arc<RasterAsset>,
    bytes: usize,
    last_used: u64,
}

#[derive(Debug)]
pub struct RasterCache {
    entries: HashMap<RasterAssetKey, CacheEntry>,
    heading_keys: HashMap<[u8; 32], RasterAssetKey>,
    table_keys: HashMap<[u8; 32], RasterAssetKey>,
    math_keys: HashMap<[u8; 32], RasterAssetKey>,
    failures: HashMap<[u8; 32], String>,
    capacity: usize,
    used: usize,
    clock: u64,
    heading_fonts: Vec<HeadingFonts>,
}

impl Default for RasterCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_BYTES)
    }
}

impl RasterCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::new_with_heading_fonts(capacity, (0..6).map(|_| HeadingFonts::default()).collect())
    }

    pub(crate) fn with_heading_fonts(heading_fonts: Vec<HeadingFonts>) -> Self {
        Self::new_with_heading_fonts(DEFAULT_CACHE_BYTES, heading_fonts)
    }

    fn new_with_heading_fonts(capacity: usize, heading_fonts: Vec<HeadingFonts>) -> Self {
        Self {
            entries: HashMap::new(),
            heading_keys: HashMap::new(),
            table_keys: HashMap::new(),
            math_keys: HashMap::new(),
            failures: HashMap::new(),
            capacity,
            used: 0,
            clock: 0,
            heading_fonts,
        }
    }

    pub(crate) fn rasterize_themed(
        &mut self,
        image: &PreparedImage,
        available_cells: (u16, u16),
        cell_pixels: PixelSize,
        theme_identity: [u8; 32],
    ) -> Result<Option<(Arc<RasterAsset>, u16, u16)>, String> {
        let Some(recipe) = image.recipe.as_ref() else {
            return Ok(None);
        };
        let target = aspect_fit(
            recipe.natural_size,
            PixelSize {
                width: u32::from(available_cells.0).saturating_mul(cell_pixels.width),
                height: u32::from(available_cells.1).saturating_mul(cell_pixels.height),
            },
        );
        let canvas_size = cell_aligned_size(target, cell_pixels);
        let Some(bytes) = raster_bytes(canvas_size) else {
            return Ok(None);
        };
        if canvas_size.width > MAX_DIMENSION
            || canvas_size.height > MAX_DIMENSION
            || bytes > self.capacity
        {
            return Ok(None);
        }
        let key = RasterAssetKey {
            content_hash: raster_cache_hash(
                recipe.content_hash,
                target,
                cell_pixels,
                theme_identity,
            ),
            pixels: canvas_size,
            rasterizer_version: RASTERIZER_VERSION,
        };
        let (columns, rows) = aligned_cells(canvas_size, cell_pixels)?;
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.clock;
            return Ok(Some((Arc::clone(&entry.asset), columns, rows)));
        }

        if let Some(error) = self.failures.get(&recipe.content_hash) {
            return Err(error.clone());
        }
        let source = match decode_image(&recipe.path, MAX_DECODED_BYTES) {
            Ok(source) => source,
            Err(error) => {
                self.failures.insert(recipe.content_hash, error.clone());
                return Err(error);
            }
        };

        let resized = image::imageops::resize(
            &source.rgba,
            target.width,
            target.height,
            FilterType::Lanczos3,
        );
        let raster = pad_to_cell_grid(resized, cell_pixels);
        let rgba: Arc<[u8]> = Arc::from(raster.into_raw());
        debug_assert_eq!(rgba.len(), bytes);
        self.evict_for(bytes);
        let asset = Arc::new(RasterAsset { key, rgba });
        self.entries.insert(
            key,
            CacheEntry {
                asset: Arc::clone(&asset),
                bytes,
                last_used: self.clock,
            },
        );
        self.used = self.used.saturating_add(bytes);
        Ok(Some((asset, columns, rows)))
    }

    #[cfg(test)]
    pub(crate) fn rasterize(
        &mut self,
        image: &PreparedImage,
        available_cells: (u16, u16),
        cell_pixels: PixelSize,
    ) -> Result<Option<(Arc<RasterAsset>, u16, u16)>, String> {
        self.rasterize_themed(image, available_cells, cell_pixels, [0; 32])
    }

    pub(crate) fn rasterize_heading_themed(
        &mut self,
        index: usize,
        heading: &HeadingElement,
        available: (u16, u16),
        cell_pixels: PixelSize,
        style: &RasterizedHeading,
        theme_identity: [u8; 32],
    ) -> Result<Option<HeadingRaster>, String> {
        let (available_columns, available_rows) = available;
        let fonts = self
            .heading_fonts
            .get(usize::from(heading.level.clamp(1, 6) - 1))
            .ok_or_else(|| "heading font set is unavailable".to_owned())?;
        let identity = HeadingPlan::cache_identity_with_fonts(
            heading,
            available_columns,
            cell_pixels,
            fonts,
            style,
            theme_identity,
        )?;
        self.clock = self.clock.saturating_add(1);
        if let Some(key) = self.heading_keys.get(&identity).copied()
            && let Some(entry) = self.entries.get_mut(&key)
        {
            let (columns, rows) = aligned_cells(key.pixels, cell_pixels)?;
            if rows > available_rows.max(1) {
                return Err(format!(
                    "heading needs {rows} terminal rows but viewport has {}",
                    available_rows.max(1)
                ));
            }
            entry.last_used = self.clock;
            return Ok(Some(HeadingRaster {
                asset: Arc::clone(&entry.asset),
                element_key: HeadingPlan::element_key(index),
                columns,
                rows,
            }));
        }
        if let Some(error) = self.failures.get(&identity) {
            return Err(error.clone());
        }
        let plan = match HeadingPlan::build_with_fonts(
            index,
            heading,
            available_columns,
            cell_pixels,
            fonts,
            style,
            theme_identity,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.failures.insert(identity, error.clone());
                return Err(error);
            }
        };
        let (columns, rows) = aligned_cells(plan.key.pixels, cell_pixels)?;
        debug_assert_eq!((plan.columns, plan.rows), (columns, rows));
        if rows > available_rows.max(1) {
            let error = format!(
                "heading needs {} terminal rows but viewport has {}",
                rows,
                available_rows.max(1)
            );
            return Err(error);
        }
        let rendered = match plan.render_with_fonts(fonts) {
            Ok(rendered) => rendered,
            Err(error) => {
                self.failures.insert(plan.key.content_hash, error.clone());
                return Err(error);
            }
        };
        let rgba: Arc<[u8]> = Arc::from(rendered.into_raw());
        let bytes = rgba.len();
        if bytes > self.capacity {
            return Err(format!(
                "heading raster needs {bytes} bytes but cache capacity is {} bytes",
                self.capacity
            ));
        }
        self.evict_for(bytes);
        let asset = Arc::new(RasterAsset {
            key: plan.key,
            rgba,
        });
        self.entries.insert(
            plan.key,
            CacheEntry {
                asset: Arc::clone(&asset),
                bytes,
                last_used: self.clock,
            },
        );
        self.heading_keys.insert(identity, plan.key);
        self.used = self.used.saturating_add(bytes);
        Ok(Some(HeadingRaster {
            asset,
            element_key: plan.element_key,
            columns,
            rows,
        }))
    }

    #[cfg(test)]
    pub(crate) fn rasterize_heading(
        &mut self,
        index: usize,
        heading: &HeadingElement,
        available_columns: u16,
        available_rows: u16,
        cell_pixels: PixelSize,
    ) -> Result<Option<HeadingRaster>, String> {
        let theme = crate::theme::Theme::builtin_dark();
        let style = theme
            .document
            .headings
            .level(heading.level)
            .rasterized()
            .expect("built-in raster heading should have raster settings");
        self.rasterize_heading_themed(
            index,
            heading,
            (available_columns, available_rows),
            cell_pixels,
            style,
            theme.identity,
        )
    }

    pub(crate) fn rasterize_table_themed(
        &mut self,
        index: usize,
        table: &TableElement,
        available_columns: u16,
        cell_pixels: PixelSize,
        style: &RasterizedTable,
        theme_identity: [u8; 32],
    ) -> Result<Option<TableRaster>, String> {
        let identity = TablePlan::cache_identity_themed(
            table,
            available_columns,
            cell_pixels,
            style,
            theme_identity,
        );
        self.clock = self.clock.saturating_add(1);
        if let Some(key) = self.table_keys.get(&identity).copied()
            && let Some(entry) = self.entries.get_mut(&key)
        {
            entry.last_used = self.clock;
            let (columns, rows) = aligned_cells(key.pixels, cell_pixels)?;
            return Ok(Some(TableRaster {
                asset: Arc::clone(&entry.asset),
                element_key: TablePlan::element_key(index),
                columns,
                rows,
            }));
        }
        if let Some(error) = self.failures.get(&identity) {
            return Err(error.clone());
        }
        let plan = match TablePlan::build_themed(
            index,
            table,
            available_columns,
            cell_pixels,
            style,
            theme_identity,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.failures.insert(identity, error.clone());
                return Err(error);
            }
        };
        let (plan, rendered) = match plan.render(cell_pixels) {
            Ok(rendered) => rendered,
            Err(error) => {
                self.failures.insert(identity, error.clone());
                return Err(error);
            }
        };
        let (columns, rows) = aligned_cells(plan.key.pixels, cell_pixels)?;
        debug_assert_eq!((plan.columns, plan.rows), (columns, rows));
        let rgba: Arc<[u8]> = Arc::from(rendered.into_raw());
        let bytes = rgba.len();
        if bytes > self.capacity {
            return Err(format!(
                "table raster needs {bytes} bytes but cache capacity is {} bytes",
                self.capacity
            ));
        }
        self.evict_for(bytes);
        let asset = Arc::new(RasterAsset {
            key: plan.key,
            rgba,
        });
        let replaced = self.entries.insert(
            plan.key,
            CacheEntry {
                asset: Arc::clone(&asset),
                bytes,
                last_used: self.clock,
            },
        );
        if let Some(replaced) = replaced {
            self.used = self.used.saturating_sub(replaced.bytes);
        }
        self.table_keys.insert(identity, plan.key);
        self.used = self.used.saturating_add(bytes);
        Ok(Some(TableRaster {
            asset,
            element_key: plan.element_key,
            columns,
            rows,
        }))
    }

    #[cfg(test)]
    pub(crate) fn rasterize_table(
        &mut self,
        index: usize,
        table: &TableElement,
        available_columns: u16,
        cell_pixels: PixelSize,
    ) -> Result<Option<TableRaster>, String> {
        let theme = crate::theme::Theme::builtin_dark();
        self.rasterize_table_themed(
            index,
            table,
            available_columns,
            cell_pixels,
            theme
                .document
                .table
                .rasterized()
                .expect("built-in table should be rasterized"),
            theme.identity,
        )
    }

    pub(crate) fn rasterize_math_themed(
        &mut self,
        index: usize,
        math: &MathElement,
        available: (u16, u16),
        cell_pixels: PixelSize,
        style: &RasterizedMath,
        theme_identity: [u8; 32],
    ) -> Result<Option<MathRaster>, String> {
        let (available_columns, available_rows) = available;
        let identity = MathPlan::cache_identity_themed(
            math,
            available_columns,
            available_rows,
            cell_pixels,
            style,
            theme_identity,
        );
        self.clock = self.clock.saturating_add(1);
        if let Some(key) = self.math_keys.get(&identity).copied()
            && let Some(entry) = self.entries.get_mut(&key)
        {
            entry.last_used = self.clock;
            let (columns, rows) = aligned_cells(key.pixels, cell_pixels)?;
            return Ok(Some(MathRaster {
                asset: Arc::clone(&entry.asset),
                element_key: MathPlan::element_key(index),
                columns,
                rows,
            }));
        }
        if let Some(error) = self.failures.get(&identity) {
            return Err(error.clone());
        }
        let plan = match MathPlan::build_themed(
            index,
            math,
            available_columns,
            available_rows,
            cell_pixels,
            style,
            theme_identity,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.failures.insert(identity, error.clone());
                return Err(error);
            }
        };
        let (plan, rendered) = match plan.render(cell_pixels) {
            Ok(rendered) => rendered,
            Err(error) => {
                self.failures.insert(identity, error.clone());
                return Err(error);
            }
        };
        let (columns, rows) = aligned_cells(plan.key.pixels, cell_pixels)?;
        debug_assert_eq!((plan.columns, plan.rows), (columns, rows));
        let rgba: Arc<[u8]> = Arc::from(rendered.into_raw());
        let bytes = rgba.len();
        if bytes > self.capacity {
            return Err(format!(
                "math raster needs {bytes} bytes but cache capacity is {} bytes",
                self.capacity
            ));
        }
        self.evict_for(bytes);
        let asset = Arc::new(RasterAsset {
            key: plan.key,
            rgba,
        });
        let replaced = self.entries.insert(
            plan.key,
            CacheEntry {
                asset: Arc::clone(&asset),
                bytes,
                last_used: self.clock,
            },
        );
        if let Some(replaced) = replaced {
            self.used = self.used.saturating_sub(replaced.bytes);
        }
        self.math_keys.insert(identity, plan.key);
        self.used = self.used.saturating_add(bytes);
        Ok(Some(MathRaster {
            asset,
            element_key: plan.element_key,
            columns,
            rows,
        }))
    }

    #[cfg(test)]
    pub(crate) fn rasterize_math(
        &mut self,
        index: usize,
        math: &MathElement,
        available_columns: u16,
        available_rows: u16,
        cell_pixels: PixelSize,
    ) -> Result<Option<MathRaster>, String> {
        let theme = crate::theme::Theme::builtin_dark();
        let element = match math.form {
            crate::MathForm::Inline => &theme.document.math.inline,
            crate::MathForm::Display => &theme.document.math.display,
        };
        self.rasterize_math_themed(
            index,
            math,
            (available_columns, available_rows),
            cell_pixels,
            element
                .rasterized()
                .expect("built-in math should be rasterized"),
            theme.identity,
        )
    }

    fn evict_for(&mut self, incoming: usize) {
        while self.used.saturating_add(incoming) > self.capacity {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.used = self.used.saturating_sub(entry.bytes);
            }
            self.heading_keys.retain(|_, mapped| *mapped != key);
            self.table_keys.retain(|_, mapped| *mapped != key);
            self.math_keys.retain(|_, mapped| *mapped != key);
        }
    }

    #[cfg(test)]
    fn contains(&self, key: RasterAssetKey) -> bool {
        self.entries.contains_key(&key)
    }
}

fn aspect_fit(source: PixelSize, bounds: PixelSize) -> PixelSize {
    let bounds_width = bounds.width.max(1);
    let bounds_height = bounds.height.max(1);
    let width_scale = f64::from(bounds_width) / f64::from(source.width);
    let height_scale = f64::from(bounds_height) / f64::from(source.height);
    let scale = width_scale.min(height_scale).min(1.0);
    PixelSize {
        width: (f64::from(source.width) * scale).round().max(1.0) as u32,
        height: (f64::from(source.height) * scale).round().max(1.0) as u32,
    }
}

pub(crate) fn cell_aligned_size(content: PixelSize, cell: PixelSize) -> PixelSize {
    PixelSize {
        width: align_to_cell(content.width, cell.width),
        height: align_to_cell(content.height, cell.height),
    }
}

pub(crate) fn pad_to_cell_grid(raster: RgbaImage, cell: PixelSize) -> RgbaImage {
    let canvas_size = cell_aligned_size(
        PixelSize {
            width: raster.width(),
            height: raster.height(),
        },
        cell,
    );
    if raster.width() == canvas_size.width && raster.height() == canvas_size.height {
        return raster;
    }

    let mut canvas = RgbaImage::new(canvas_size.width, canvas_size.height);
    image::imageops::replace(&mut canvas, &raster, 0, 0);
    canvas
}

fn align_to_cell(pixels: u32, cell: u32) -> u32 {
    let cell = cell.max(1);
    pixels.max(1).div_ceil(cell).saturating_mul(cell)
}

fn aligned_cells(pixels: PixelSize, cell: PixelSize) -> Result<(u16, u16), String> {
    let cell_width = cell.width.max(1);
    let cell_height = cell.height.max(1);
    if pixels.width == 0
        || pixels.height == 0
        || !pixels.width.is_multiple_of(cell_width)
        || !pixels.height.is_multiple_of(cell_height)
    {
        return Err(format!(
            "raster {}x{} does not align to terminal cells {}x{}",
            pixels.width, pixels.height, cell_width, cell_height
        ));
    }
    let columns = u16::try_from(pixels.width / cell_width)
        .map_err(|_| "raster width exceeds the terminal cell range".to_owned())?;
    let rows = u16::try_from(pixels.height / cell_height)
        .map_err(|_| "raster height exceeds the terminal cell range".to_owned())?;
    Ok((columns.max(1), rows.max(1)))
}

fn raster_cache_hash(
    content_hash: [u8; 32],
    content: PixelSize,
    cell: PixelSize,
    theme_identity: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&content_hash);
    hasher.update(&content.width.to_le_bytes());
    hasher.update(&content.height.to_le_bytes());
    hasher.update(&cell.width.to_le_bytes());
    hasher.update(&cell.height.to_le_bytes());
    hasher.update(&theme_identity);
    *hasher.finalize().as_bytes()
}

fn raster_bytes(pixels: PixelSize) -> Option<usize> {
    usize::try_from(pixels.width)
        .ok()?
        .checked_mul(usize::try_from(pixels.height).ok()?)?
        .checked_mul(4)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use image::{ImageBuffer, Rgba};

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "raden-image-{}-{suffix}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("fixture directory should be created");
        path
    }

    #[test]
    fn local_resolution_rejects_remote_and_root_escape() {
        let root = temp_dir();
        assert!(resolve_local_path(&root, "https://example.com/a.png").is_err());
        assert!(resolve_local_path(&root, "../outside.png").is_err());
        assert!(resolve_local_path(&root, "%2e%2e/outside.png").is_err());
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn bare_markdown_filename_uses_the_current_directory() {
        assert_eq!(
            resolve_markdown_root(Path::new("doc.md")).expect("cwd should resolve"),
            fs::canonicalize(".").expect("cwd should canonicalize")
        );
    }

    #[test]
    fn catalog_decodes_supported_image_and_reports_missing_image() {
        let root = temp_dir();
        let image_path = root.join("pixel.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(2, 1, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("PNG fixture should be saved");
        let markdown_path = root.join("doc.md");
        let document = Document::parse("![ok](pixel.png)\n\n![lost](missing.png)".to_owned());
        let catalog = ImageCatalog::load(&document, &markdown_path);

        assert!(catalog.get(0).is_some_and(PreparedImage::can_render));
        assert!(catalog.get(1).is_some_and(|image| !image.can_render()));
        assert_eq!(catalog.diagnostics().len(), 1);
        fs::remove_file(image_path).expect("fixture image should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn selected_codec_subset_decodes_and_oversized_dimensions_fail() {
        let root = temp_dir();
        let source =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255])));
        for (name, format) in [
            ("image.png", ImageFormat::Png),
            ("image.jpg", ImageFormat::Jpeg),
            ("image.webp", ImageFormat::WebP),
            ("image.gif", ImageFormat::Gif),
        ] {
            let path = root.join(name);
            source
                .save_with_format(&path, format)
                .expect("supported fixture should encode");
            assert!(
                decode_image(&path, MAX_DECODED_BYTES).is_ok(),
                "{name} should decode"
            );
            let document = Document::parse(format!("![fixture]({name})"));
            let catalog = ImageCatalog::load(&document, &root.join("doc.md"));
            let mut cache = RasterCache::new(1_024);
            let cell = PixelSize {
                width: 1,
                height: 1,
            };
            let layout =
                crate::Layout::with_graphics(&document, 10, 10, &catalog, &mut cache, cell, 0);
            assert_eq!(layout.graphics.len(), 1, "{name} should reach the view");
            fs::remove_file(path).expect("fixture should be removable");
        }

        let oversized = root.join("oversized.png");
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            MAX_DIMENSION + 1,
            1,
            image::Rgba([0, 0, 0, 255]),
        ))
        .save(&oversized)
        .expect("oversized header fixture should encode");
        assert!(decode_image(&oversized, MAX_DECODED_BYTES).is_err());
        fs::remove_file(oversized).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn animated_gif_uses_only_its_first_frame() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};

        let root = temp_dir();
        let path = root.join("animated.gif");
        let file = fs::File::create(&path).expect("GIF fixture should be created");
        let first = Frame::from_parts(
            RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(100, 1),
        );
        let second = Frame::from_parts(
            RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 255, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(100, 1),
        );
        GifEncoder::new(file)
            .encode_frames([first, second])
            .expect("animated GIF should encode");

        let decoded = decode_image(&path, MAX_DECODED_BYTES).expect("animated GIF should decode");
        assert_eq!(decoded.rgba.get_pixel(0, 0).0, [255, 0, 0, 255]);
        fs::remove_file(path).expect("fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[cfg(unix)]
    #[test]
    fn local_resolution_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir();
        let outside = root.with_extension("outside.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(1, 1, Rgba([1, 2, 3, 255]))
            .save(&outside)
            .expect("outside fixture should be saved");
        symlink(&outside, root.join("linked.png")).expect("symlink should be created");

        assert!(resolve_local_path(&root, "linked.png").is_err());
        fs::remove_file(root.join("linked.png")).expect("symlink should be removable");
        fs::remove_file(outside).expect("outside fixture should be removable");
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn cache_preserves_aspect_ratio_and_evicts_least_recent_asset() {
        let source_a = RasterSource {
            rgba: RgbaImage::from_pixel(4, 2, image::Rgba([1, 2, 3, 255])),
        };
        let source_b = RasterSource {
            rgba: RgbaImage::from_pixel(4, 2, image::Rgba([4, 5, 6, 255])),
        };
        let root = temp_dir();
        let prepared = |key, raster: RasterSource| {
            let path = root.join(format!("cache-{key}.png"));
            DynamicImage::ImageRgba8(raster.rgba)
                .save(&path)
                .expect("cache fixture should save");
            PreparedImage {
                element_key: ElementKey(key),
                source: 0..1,
                label: format!("cache-{key}"),
                text_projection: String::new(),
                style: TextStyle::default(),
                recipe: Some(inspect_image(path).expect("fixture should inspect")),
            }
        };
        let a = prepared(1, source_a);
        let b = prepared(2, source_b);
        let mut cache = RasterCache::new(32);
        let cell = PixelSize {
            width: 1,
            height: 1,
        };
        let (asset_a, cols, rows) = cache
            .rasterize(&a, (4, 4), cell)
            .expect("first image should decode")
            .expect("first image should fit cache");
        assert_eq!((cols, rows), (4, 2));
        let key_a = asset_a.key;
        let (asset_b, _, _) = cache
            .rasterize(&b, (4, 4), cell)
            .expect("second image should decode")
            .expect("second image should fit after eviction");

        assert!(!cache.contains(key_a));
        assert!(cache.contains(asset_b.key));
        fs::remove_dir_all(root).expect("fixture directory should be removable");
    }

    #[test]
    fn rasterized_images_pad_to_cells_without_resizing_the_content() {
        let root = temp_dir();
        let path = root.join("unaligned.png");
        RgbaImage::from_pixel(13, 9, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .expect("fixture should save");
        let prepared = PreparedImage {
            element_key: ElementKey(1),
            source: 0..1,
            label: "unaligned".to_owned(),
            text_projection: String::new(),
            style: TextStyle::default(),
            recipe: Some(inspect_image(path).expect("fixture should inspect")),
        };
        let mut cache = RasterCache::default();
        let mut keys = Vec::new();

        for cell in [
            PixelSize {
                width: 8,
                height: 16,
            },
            PixelSize {
                width: 10,
                height: 6,
            },
        ] {
            let (asset, columns, rows) = cache
                .rasterize(&prepared, (4, 4), cell)
                .expect("image should rasterize")
                .expect("image should fit the cache");
            assert_eq!(asset.key.pixels.width, u32::from(columns) * cell.width);
            assert_eq!(asset.key.pixels.height, u32::from(rows) * cell.height);

            let raster = RgbaImage::from_raw(
                asset.key.pixels.width,
                asset.key.pixels.height,
                asset.rgba.to_vec(),
            )
            .expect("cached raster dimensions should match its pixels");
            assert!(
                raster
                    .enumerate_pixels()
                    .filter(|(x, y, _)| *x < 13 && *y < 9)
                    .all(|(_, _, pixel)| pixel.0 == [10, 20, 30, 255]),
                "cell alignment must not rescale the source content"
            );
            assert!(
                raster
                    .enumerate_pixels()
                    .filter(|(x, y, _)| *x >= 13 || *y >= 9)
                    .all(|(_, _, pixel)| pixel.0[3] == 0),
                "cell alignment gaps must remain transparent"
            );
            keys.push(asset.key);
        }

        assert_ne!(keys[0], keys[1], "cell geometry belongs in cache identity");
        fs::remove_dir_all(root).expect("fixture directory should be removable");
    }

    #[test]
    fn rasterized_images_reject_oversized_aligned_canvases_before_allocation() {
        let root = temp_dir();
        let path = root.join("pixel.png");
        RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .expect("fixture should save");
        let prepared = PreparedImage {
            element_key: ElementKey(1),
            source: 0..1,
            label: "pixel".to_owned(),
            text_projection: String::new(),
            style: TextStyle::default(),
            recipe: Some(inspect_image(path).expect("fixture should inspect")),
        };
        let mut cache = RasterCache::default();

        let rendered = cache
            .rasterize(
                &prepared,
                (1, 1),
                PixelSize {
                    width: u32::from(u16::MAX),
                    height: u32::from(u16::MAX),
                },
            )
            .expect("oversized alignment should use the existing fallback path");

        assert!(rendered.is_none());
        assert_eq!(cache.used, 0);
        fs::remove_dir_all(root).expect("fixture directory should be removable");
    }

    #[test]
    fn theme_identity_separates_raster_cache_entries() {
        let root = temp_dir();
        let path = root.join("pixel.png");
        RgbaImage::from_pixel(2, 1, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .expect("fixture should save");
        let prepared = PreparedImage {
            element_key: ElementKey(1),
            source: 0..1,
            label: "pixel".to_owned(),
            text_projection: String::new(),
            style: TextStyle::default(),
            recipe: Some(inspect_image(path).expect("fixture should inspect")),
        };
        let mut cache = RasterCache::default();

        let first = cache
            .rasterize_themed(&prepared, (4, 2), PixelSize::FALLBACK, [1; 32])
            .expect("first Theme should rasterize")
            .expect("first raster should exist");
        let second = cache
            .rasterize_themed(&prepared, (4, 2), PixelSize::FALLBACK, [2; 32])
            .expect("second Theme should rasterize")
            .expect("second raster should exist");

        assert_ne!(first.0.key, second.0.key);
        assert_eq!(cache.entries.len(), 2);
        fs::remove_dir_all(root).expect("fixture directory should be removable");
    }

    #[test]
    fn generated_raster_metadata_matches_between_cache_miss_and_hit() {
        let document = Document::parse(
            "# Heading\n\n$x^2$\n\n| a | b |\n| --- | --- |\n| c | d |\n".to_owned(),
        );
        let cell = PixelSize {
            width: 9,
            height: 17,
        };
        let mut cache = RasterCache::default();

        let heading_first = cache
            .rasterize_heading(0, &document.headings()[0], 40, 20, cell)
            .expect("heading should rasterize")
            .expect("heading raster should exist");
        let heading_cached = cache
            .rasterize_heading(0, &document.headings()[0], 40, 20, cell)
            .expect("cached heading should load")
            .expect("cached heading raster should exist");
        assert_eq!(
            (heading_first.columns, heading_first.rows),
            (heading_cached.columns, heading_cached.rows)
        );

        let math_first = cache
            .rasterize_math(0, &document.math()[0], 40, 20, cell)
            .expect("math should rasterize")
            .expect("math raster should exist");
        let math_cached = cache
            .rasterize_math(0, &document.math()[0], 40, 20, cell)
            .expect("cached math should load")
            .expect("cached math raster should exist");
        assert_eq!(
            (math_first.columns, math_first.rows),
            (math_cached.columns, math_cached.rows)
        );

        let table_first = cache
            .rasterize_table(0, &document.tables()[0], 40, cell)
            .expect("table should rasterize")
            .expect("table raster should exist");
        let table_cached = cache
            .rasterize_table(0, &document.tables()[0], 40, cell)
            .expect("cached table should load")
            .expect("cached table raster should exist");
        assert_eq!(
            (table_first.columns, table_first.rows),
            (table_cached.columns, table_cached.rows)
        );
    }

    #[test]
    fn urushi_image_owns_raster_after_cache_eviction() {
        let root = temp_dir();
        let prepared = |key| {
            let path = root.join(format!("frame-{key}.png"));
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                4,
                2,
                image::Rgba([key as u8, 0, 0, 255]),
            ))
            .save(&path)
            .expect("frame fixture should save");
            PreparedImage {
                element_key: ElementKey(key),
                source: 0..1,
                label: format!("frame-{key}"),
                text_projection: String::new(),
                style: TextStyle::default(),
                recipe: Some(inspect_image(path).expect("fixture should inspect")),
            }
        };
        let mut cache = RasterCache::new(32);
        let cell = PixelSize {
            width: 1,
            height: 1,
        };
        let (first, _, _) = cache
            .rasterize(&prepared(1), (4, 2), cell)
            .expect("first asset should decode")
            .expect("first asset should fit");
        let first_key = first.key;
        let image = urushi_graphics::Image::rgba(
            urushi::Key::of(&ElementKey(1)),
            urushi::Key::of(&first_key),
            urushi_graphics::PixelSize::new(first_key.pixels.width, first_key.pixels.height),
            first.rgba.clone(),
        )
        .expect("cache assets are checked RGBA rasters");

        cache
            .rasterize(&prepared(2), (4, 2), cell)
            .expect("second asset should decode")
            .expect("second asset should evict first");

        assert!(!cache.contains(first_key));
        assert_eq!(image.raster().bytes().len(), 32);
        fs::remove_dir_all(root).expect("fixture directory should be removable");
    }

    #[test]
    fn decode_failure_is_actionable_and_negative_cached() {
        let root = temp_dir();
        let path = root.join("corrupt.png");
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255])))
            .save(&path)
            .expect("valid PNG fixture should save");
        let mut bytes = fs::read(&path).expect("fixture should be readable");
        let idat = bytes
            .windows(4)
            .position(|window| window == b"IDAT")
            .expect("PNG fixture should contain pixel data");
        bytes[idat + 4] ^= 0xff;
        fs::write(&path, bytes).expect("fixture pixel data should be corrupted");

        let prepared = PreparedImage {
            element_key: ElementKey(1),
            source: 0..1,
            label: "corrupt".to_owned(),
            text_projection: "corrupt".to_owned(),
            style: TextStyle::default(),
            recipe: Some(inspect_image(path.clone()).expect("PNG header should expose dimensions")),
        };
        let mut cache = RasterCache::default();
        let cell = PixelSize {
            width: 1,
            height: 1,
        };
        let first_error = cache
            .rasterize(&prepared, (2, 1), cell)
            .expect_err("truncated pixel payload should fail to decode");
        assert!(
            prepared
                .failure_projection(&first_error)
                .contains("unavailable: corrupt")
        );

        let document = Document::parse("![corrupt](corrupt.png)".to_owned());
        let catalog = ImageCatalog {
            images: vec![prepared.clone()],
            diagnostics: Vec::new(),
        };
        let layout = crate::Layout::with_graphics(&document, 80, 10, &catalog, &mut cache, cell, 0);
        let rendered = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("image: unavailable: corrupt"));
        assert!(rendered.contains("could not decode image"));

        fs::remove_file(&path).expect("fixture should be removable after the first attempt");
        let cached_error = cache
            .rasterize(&prepared, (2, 1), cell)
            .expect_err("decode failure should remain cached without reopening the file");
        assert_eq!(cached_error, first_error);
        assert_eq!(cache.failures.len(), 1);
        fs::remove_dir(root).expect("fixture directory should be removable");
    }

    #[test]
    fn table_cache_separates_content_sized_rasters_by_cell_height() {
        let document =
            Document::parse("| a | b |\n| --- | --- |\n| wrapped value | 2 |\n".to_owned());
        let table = &document.tables()[0];
        let mut cache = RasterCache::default();
        let first = cache
            .rasterize_table(
                0,
                table,
                40,
                PixelSize {
                    width: 8,
                    height: 16,
                },
            )
            .expect("first table should rasterize")
            .expect("table raster exists");
        let first_bytes = first.asset.rgba.len();
        assert_eq!(cache.used, first_bytes);

        let second = cache
            .rasterize_table(
                0,
                table,
                40,
                PixelSize {
                    width: 8,
                    height: 20,
                },
            )
            .expect("same-width table should rasterize at new cell height")
            .expect("table raster exists");
        assert_ne!(second.asset.key, first.asset.key);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.used, first.asset.rgba.len() + second.asset.rgba.len());
    }
}
