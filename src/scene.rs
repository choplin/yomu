use std::collections::HashMap;
use std::sync::Arc;

use crate::{ElementKey, PixelSize, RasterAsset, RasterAssetKey, SourceSpan};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// A source-mapped subregion within a graphic, relative to its target origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicSourceRegion {
    pub target: CellRect,
    pub source: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicPlacement {
    pub element_key: ElementKey,
    pub asset_key: RasterAssetKey,
    pub target: CellRect,
    pub clip: CellRect,
    pub source_y: u32,
    pub source: SourceSpan,
    pub source_regions: Vec<GraphicSourceRegion>,
}

impl GraphicPlacement {
    #[must_use]
    pub fn source_at(&self, column: u16, row: u16) -> SourceSpan {
        self.source_regions
            .iter()
            .find(|region| {
                region.target.x <= column
                    && column < region.target.x.saturating_add(region.target.width)
                    && region.target.y <= row
                    && row < region.target.y.saturating_add(region.target.height)
            })
            .map(|region| region.source.clone())
            .unwrap_or_else(|| self.source.clone())
    }
}

#[derive(Clone, Debug)]
pub struct DesiredFrame {
    pub generation: u64,
    pub cell_pixels: PixelSize,
    pub placements: Vec<GraphicPlacement>,
    pub assets: HashMap<RasterAssetKey, Arc<RasterAsset>>,
}
