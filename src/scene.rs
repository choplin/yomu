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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicPlacement {
    pub element_key: ElementKey,
    pub asset_key: RasterAssetKey,
    pub target: CellRect,
    pub clip: CellRect,
    pub source_y: u32,
    pub source: SourceSpan,
}

#[derive(Clone, Debug)]
pub struct DesiredFrame {
    pub generation: u64,
    pub cell_pixels: PixelSize,
    pub placements: Vec<GraphicPlacement>,
    pub assets: HashMap<RasterAssetKey, Arc<RasterAsset>>,
}
