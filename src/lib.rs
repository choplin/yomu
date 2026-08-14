//! Core document, layout, and application state for raden.

mod app;
mod config;
mod document;
mod graphics;
mod heading;
mod image;
mod layout;
mod math;
mod scene;
mod table;
mod terminal;
mod ui;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub use app::{App, Command};
pub use document::{
    Document, HeadingElement, ImageElement, MathElement, MathForm, SourceEvent, SourceSpan,
    TableAlignment, TableCell, TableElement, TableRow, TextStyle,
};
pub(crate) use image::{ElementKey, PixelSize, RasterAsset, RasterAssetKey, RasterCache};
pub use layout::{Layout, LayoutFragment, LayoutRow};
pub(crate) use scene::{CellRect, DesiredFrame, GraphicPlacement, GraphicSourceRegion};

/// Load and parse a UTF-8 Markdown document from disk.
pub fn load_document(path: &Path) -> Result<Document> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("could not read Markdown file {}", path.display()))?;
    Ok(Document::parse(source))
}

/// Open one Markdown document in the interactive terminal viewer.
pub fn run(path: &Path) -> Result<()> {
    let config = config::Config::load()?;
    let document = load_document(path)?;
    terminal::run(path, &document, &config)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn load_document_reports_invalid_utf8_with_the_path() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("raden-invalid-{suffix}.md"));
        fs::write(&path, [0xff, 0xfe]).expect("fixture should be writable");

        let error = load_document(&path).expect_err("invalid UTF-8 must fail");
        let message = format!("{error:#}");

        fs::remove_file(&path).expect("fixture should be removable");
        assert!(message.contains("could not read Markdown file"));
        assert!(message.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn load_document_reports_a_missing_path() {
        let path = std::env::temp_dir().join("raden-file-that-does-not-exist.md");
        let error = load_document(&path).expect_err("missing input must fail");
        let message = format!("{error:#}");

        assert!(message.contains("could not read Markdown file"));
        assert!(message.contains(path.to_string_lossy().as_ref()));
    }
}
