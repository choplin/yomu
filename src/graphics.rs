use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::ValueEnum;
use icy_sixel::{EncodeOptions, QuantizeMethod, sixel_encode};
use image::RgbaImage;
use ratatui::layout::Size;
use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions, Response};
use serde::Deserialize;

use crate::{DesiredFrame, ElementKey, GraphicPlacement, PixelSize, RasterAsset, RasterAssetKey};

const RAW_CHUNK_BYTES: usize = 3_072;
const MAX_UPLOADED_IMAGES: usize = 64;
const MAX_ENCODED_SIXELS: usize = 64;
const SIXEL_MAX_COLORS: u16 = 16;

/// User-selected graphics policy. Automatic selection prefers Kitty, then sixel,
/// and finally the complete text projection.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsPreference {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsBackendKind {
    Kitty,
    Sixel,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphicsSelection {
    pub(crate) backend: GraphicsBackendKind,
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CapabilityEvidence {
    graphics_query_authoritative: bool,
    kitty: bool,
    sixel: bool,
    kitty_hint: bool,
    sixel_hint: bool,
    pub(crate) cell_pixels: Option<PixelSize>,
}

impl CapabilityEvidence {
    pub(crate) fn environment_fallback() -> Self {
        Self {
            kitty_hint: kitty_environment_hint(),
            sixel_hint: sixel_environment_hint(),
            ..Self::default()
        }
    }

    pub(crate) fn query_stdio() -> io::Result<Self> {
        let mut input = terminal_input()?;
        Self::query(&mut input)
    }

    #[cfg(unix)]
    fn query(input: &mut (impl Read + std::os::fd::AsFd)) -> io::Result<Self> {
        Self::query_inner(input)
    }

    #[cfg(windows)]
    fn query(input: &mut (impl Read + std::os::windows::io::AsRawHandle)) -> io::Result<Self> {
        Self::query_inner(input)
    }

    fn query_inner(input: &mut impl TerminalRead) -> io::Result<Self> {
        let options = QueryStdioOptions::default();
        let deadline = Instant::now() + options.timeout;
        let query = Parser::query(inside_tmux(), options);
        let mut stdout = io::stdout().lock();
        stdout.write_all(query.as_bytes())?;
        stdout.flush()?;
        drop(stdout);

        let mut parser = Parser::new();
        let mut evidence = Self::environment_fallback();
        let mut buffer = [0u8; 256];
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if !stdin_ready(&input, remaining)? {
                return Ok(evidence);
            }
            let read = input.read(&mut buffer)?;
            if read == 0 {
                return Ok(evidence);
            }
            for byte in &buffer[..read] {
                for response in parser.push(char::from(*byte)) {
                    match response {
                        Response::Kitty => evidence.kitty = true,
                        Response::Sixel => evidence.sixel = true,
                        Response::CellSize(Some((width, height))) if width > 0 && height > 0 => {
                            evidence.cell_pixels = Some(PixelSize {
                                width: u32::from(width),
                                height: u32::from(height),
                            });
                        }
                        Response::Status => {
                            evidence.graphics_query_authoritative = true;
                            return Ok(evidence);
                        }
                        Response::RectangularOps
                        | Response::CellSize(None)
                        | Response::CellSize(Some(_))
                        | Response::CursorPositionReport(_, _)
                        | Response::Background(_, _, _) => {}
                    }
                }
            }
        }
        Ok(evidence)
    }

    fn supports(&self, backend: GraphicsBackendKind) -> bool {
        match backend {
            GraphicsBackendKind::Text => true,
            GraphicsBackendKind::Kitty if self.kitty => true,
            GraphicsBackendKind::Sixel if self.sixel => true,
            GraphicsBackendKind::Kitty if !self.graphics_query_authoritative => self.kitty_hint,
            GraphicsBackendKind::Sixel if !self.graphics_query_authoritative => self.sixel_hint,
            GraphicsBackendKind::Kitty | GraphicsBackendKind::Sixel => false,
        }
    }

    fn unavailable_reason(&self) -> &'static str {
        if self.graphics_query_authoritative {
            "the terminal capability response did not report support"
        } else {
            "the terminal capability query returned no usable response or fallback hint"
        }
    }

    fn selection_reason(&self, backend: GraphicsBackendKind) -> Option<String> {
        let selected_from_hint = !self.graphics_query_authoritative
            && match backend {
                GraphicsBackendKind::Kitty => !self.kitty && self.kitty_hint,
                GraphicsBackendKind::Sixel => !self.sixel && self.sixel_hint,
                GraphicsBackendKind::Text => false,
            };
        selected_from_hint.then(|| {
            "selected from an environment fallback hint after no authoritative graphics response"
                .to_owned()
        })
    }
}

#[cfg(unix)]
trait TerminalRead: Read + std::os::fd::AsFd {}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> TerminalRead for T {}

#[cfg(windows)]
trait TerminalRead: Read + std::os::windows::io::AsRawHandle {}

#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> TerminalRead for T {}

#[cfg(unix)]
fn terminal_input() -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

#[cfg(windows)]
fn terminal_input() -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open("CONIN$")
}

pub(crate) fn select_graphics_backend(
    preference: GraphicsPreference,
    evidence: CapabilityEvidence,
    needs_graphics: bool,
) -> Result<GraphicsSelection, String> {
    if preference == GraphicsPreference::Text {
        return Ok(GraphicsSelection {
            backend: GraphicsBackendKind::Text,
            reason: None,
        });
    }

    let explicit = match preference {
        GraphicsPreference::Kitty => Some(GraphicsBackendKind::Kitty),
        GraphicsPreference::Sixel => Some(GraphicsBackendKind::Sixel),
        GraphicsPreference::Auto | GraphicsPreference::Text => None,
    };
    if let Some(backend) = explicit {
        if evidence.supports(backend) {
            return Ok(GraphicsSelection {
                backend,
                reason: evidence.selection_reason(backend),
            });
        }
        let name = match backend {
            GraphicsBackendKind::Kitty => "kitty",
            GraphicsBackendKind::Sixel => "sixel",
            GraphicsBackendKind::Text => "text",
        };
        return Err(format!(
            "requested graphics backend '{name}' is unavailable: {}; use --graphics auto or --graphics text",
            evidence.unavailable_reason()
        ));
    }

    if !needs_graphics {
        return Ok(GraphicsSelection {
            backend: GraphicsBackendKind::Text,
            reason: None,
        });
    }

    if evidence.supports(GraphicsBackendKind::Kitty) {
        return Ok(GraphicsSelection {
            backend: GraphicsBackendKind::Kitty,
            reason: evidence.selection_reason(GraphicsBackendKind::Kitty),
        });
    }
    if evidence.supports(GraphicsBackendKind::Sixel) {
        return Ok(GraphicsSelection {
            backend: GraphicsBackendKind::Sixel,
            reason: evidence.selection_reason(GraphicsBackendKind::Sixel),
        });
    }
    Ok(GraphicsSelection {
        backend: GraphicsBackendKind::Text,
        reason: Some(if evidence.graphics_query_authoritative {
            "terminal reported no Kitty or sixel support".to_owned()
        } else {
            "capability query returned no usable graphics response".to_owned()
        }),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Uploaded {
    image_id: u32,
    last_used: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VisiblePlacement {
    placement_id: u32,
    image_id: u32,
    placement: GraphicPlacement,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum KittyOperation {
    Upload {
        image_id: u32,
        asset_key: RasterAssetKey,
    },
    DeletePlacement {
        image_id: u32,
        placement_id: u32,
    },
    Place {
        image_id: u32,
        placement_id: u32,
        placement: GraphicPlacement,
    },
    DeleteImage {
        image_id: u32,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KittyBackend {
    uploaded: HashMap<RasterAssetKey, Uploaded>,
    visible: HashMap<ElementKey, VisiblePlacement>,
    next_image_id: u32,
    next_placement_id: u32,
}

impl KittyBackend {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            next_image_id: 1,
            next_placement_id: 1,
            ..Self::default()
        }
    }

    pub(crate) fn render<W: Write>(
        &mut self,
        frame: &DesiredFrame,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut candidate = self.clone();
        let operations = candidate.reconcile(frame);
        for operation in operations {
            if let Err(error) = candidate.write_operation(operation, frame, writer) {
                self.retain_possible_uploads(&candidate);
                return Err(error);
            }
        }
        if let Err(error) = writer.flush() {
            self.retain_possible_uploads(&candidate);
            return Err(error);
        }
        *self = candidate;
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        for uploaded in self.uploaded.values() {
            write!(writer, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", uploaded.image_id)?;
        }
        writer.flush()?;
        self.uploaded.clear();
        self.visible.clear();
        Ok(())
    }

    fn reconcile(&mut self, frame: &DesiredFrame) -> Vec<KittyOperation> {
        let mut operations = Vec::new();
        let mut superseded_images = HashSet::new();
        let desired_elements = frame
            .placements
            .iter()
            .map(|placement| placement.element_key)
            .collect::<HashSet<_>>();
        let stale = self
            .visible
            .keys()
            .filter(|key| !desired_elements.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(placement) = self.visible.remove(&key) {
                operations.push(KittyOperation::DeletePlacement {
                    image_id: placement.image_id,
                    placement_id: placement.placement_id,
                });
            }
        }

        for desired in &frame.placements {
            let uploaded = if let Some(uploaded) = self.uploaded.get_mut(&desired.asset_key) {
                uploaded.last_used = frame.generation;
                *uploaded
            } else {
                let image_id = self.allocate_image_id();
                let uploaded = Uploaded {
                    image_id,
                    last_used: frame.generation,
                };
                self.uploaded.insert(desired.asset_key, uploaded);
                operations.push(KittyOperation::Upload {
                    image_id,
                    asset_key: desired.asset_key,
                });
                uploaded
            };

            match self.visible.get(&desired.element_key) {
                Some(current)
                    if current.image_id == uploaded.image_id && current.placement == *desired => {}
                Some(current) => {
                    if current.image_id != uploaded.image_id {
                        superseded_images.insert(current.image_id);
                        operations.push(KittyOperation::DeletePlacement {
                            image_id: current.image_id,
                            placement_id: current.placement_id,
                        });
                    }
                    let placement_id = current.placement_id;
                    operations.push(KittyOperation::Place {
                        image_id: uploaded.image_id,
                        placement_id,
                        placement: desired.clone(),
                    });
                    self.visible.insert(
                        desired.element_key,
                        VisiblePlacement {
                            placement_id,
                            image_id: uploaded.image_id,
                            placement: desired.clone(),
                        },
                    );
                }
                None => {
                    let placement_id = self.allocate_placement_id();
                    operations.push(KittyOperation::Place {
                        image_id: uploaded.image_id,
                        placement_id,
                        placement: desired.clone(),
                    });
                    self.visible.insert(
                        desired.element_key,
                        VisiblePlacement {
                            placement_id,
                            image_id: uploaded.image_id,
                            placement: desired.clone(),
                        },
                    );
                }
            }
        }

        let referenced = self
            .visible
            .values()
            .map(|placement| placement.image_id)
            .collect::<HashSet<_>>();
        let mut unused = self
            .uploaded
            .iter()
            .filter(|(_, uploaded)| !referenced.contains(&uploaded.image_id))
            .map(|(key, uploaded)| (*key, *uploaded))
            .collect::<Vec<_>>();
        unused.sort_by_key(|(_, uploaded)| uploaded.last_used);
        let excess = self.uploaded.len().saturating_sub(MAX_UPLOADED_IMAGES);
        let evicted = unused
            .iter()
            .take(excess)
            .map(|(_, uploaded)| uploaded.image_id)
            .collect::<HashSet<_>>();
        for (key, uploaded) in unused {
            if !superseded_images.contains(&uploaded.image_id)
                && !evicted.contains(&uploaded.image_id)
            {
                continue;
            }
            self.uploaded.remove(&key);
            operations.push(KittyOperation::DeleteImage {
                image_id: uploaded.image_id,
            });
        }
        operations
    }

    fn write_operation<W: Write>(
        &self,
        operation: KittyOperation,
        frame: &DesiredFrame,
        writer: &mut W,
    ) -> io::Result<()> {
        match operation {
            KittyOperation::Upload {
                image_id,
                asset_key,
            } => {
                let asset = frame.assets.get(&asset_key).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "desired raster asset is missing",
                    )
                })?;
                write_upload(writer, image_id, asset)
            }
            KittyOperation::DeletePlacement {
                image_id,
                placement_id,
            } => write!(
                writer,
                "\x1b_Ga=d,d=i,i={image_id},p={placement_id},q=2\x1b\\"
            ),
            KittyOperation::Place {
                image_id,
                placement_id,
                placement,
            } => write_placement(writer, image_id, placement_id, &placement, frame),
            KittyOperation::DeleteImage { image_id } => {
                write!(writer, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
            }
        }
    }

    fn allocate_image_id(&mut self) -> u32 {
        let id = self.next_image_id.max(1);
        self.next_image_id = id.checked_add(1).unwrap_or(1);
        id
    }

    fn allocate_placement_id(&mut self) -> u32 {
        let id = self.next_placement_id.max(1);
        self.next_placement_id = id.checked_add(1).unwrap_or(1);
        id
    }

    fn retain_possible_uploads(&mut self, candidate: &Self) {
        for (key, uploaded) in &candidate.uploaded {
            self.uploaded.entry(*key).or_insert(*uploaded);
        }
        self.next_image_id = self.next_image_id.max(candidate.next_image_id);
        self.next_placement_id = self.next_placement_id.max(candidate.next_placement_id);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SixelCacheKey {
    asset_key: RasterAssetKey,
    source_y: u32,
    visible_width: u32,
    visible_height: u32,
    cells: Size,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EncodedSixel {
    data: Arc<str>,
    last_used: u64,
}

/// Immediate-mode sixel renderer. Encoded pixels are reusable, but terminal
/// placement state is deliberately not retained across frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct SixelBackend {
    encoded: HashMap<SixelCacheKey, EncodedSixel>,
}

impl SixelBackend {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn render<W: Write>(
        &mut self,
        frame: &DesiredFrame,
        writer: &mut W,
    ) -> io::Result<()> {
        for placement in &frame.placements {
            for band in 0..placement.clip.height {
                let encoded = self.encoded_band(frame, placement, band)?;
                write!(
                    writer,
                    "\x1b7\x1b[{};{}H{}\x1b8",
                    placement.clip.y.saturating_add(band).saturating_add(1),
                    placement.clip.x.saturating_add(1),
                    encoded
                )?;
            }
        }
        writer.flush()
    }

    /// Sixel has no reliable portable partial-delete operation. Clearing the
    /// alternate screen makes the caller redraw text and every desired image.
    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write!(writer, "\x1b[2J\x1b[H")?;
        writer.flush()
    }

    fn encoded_band(
        &mut self,
        frame: &DesiredFrame,
        placement: &GraphicPlacement,
        band: u16,
    ) -> io::Result<Arc<str>> {
        let asset = frame.assets.get(&placement.asset_key).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "desired raster asset is missing",
            )
        })?;
        let key = sixel_cache_key(frame, placement, band).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "desired sixel asset is missing")
        })?;
        if let Some(encoded) = self.encoded.get_mut(&key) {
            encoded.last_used = frame.generation;
            return Ok(Arc::clone(&encoded.data));
        }

        let image = visible_image(asset, key.source_y, key.visible_width, key.visible_height)?;
        let options = EncodeOptions {
            max_colors: SIXEL_MAX_COLORS,
            diffusion: 0.0,
            quantize_method: QuantizeMethod::Wu,
        };
        let data = sixel_encode(
            image.as_raw(),
            image.width() as usize,
            image.height() as usize,
            &options,
        )
        .map_err(|error| io::Error::other(format!("could not encode sixel: {error}")))?;
        let data = Arc::<str>::from(data);
        self.encoded.insert(
            key,
            EncodedSixel {
                data: Arc::clone(&data),
                last_used: frame.generation,
            },
        );
        self.evict_unused(frame, key);
        Ok(data)
    }

    fn evict_unused(&mut self, frame: &DesiredFrame, inserted: SixelCacheKey) {
        let excess = self.encoded.len().saturating_sub(MAX_ENCODED_SIXELS);
        if excess == 0 {
            return;
        }
        let live = frame
            .placements
            .iter()
            .flat_map(|placement| {
                (0..placement.clip.height)
                    .filter_map(|band| sixel_cache_key(frame, placement, band))
            })
            .chain(std::iter::once(inserted))
            .collect::<HashSet<_>>();
        let mut unused = self
            .encoded
            .iter()
            .filter(|(key, _)| !live.contains(key))
            .map(|(key, encoded)| (*key, encoded.last_used))
            .collect::<Vec<_>>();
        unused.sort_by_key(|(_, last_used)| *last_used);
        for (key, _) in unused.into_iter().take(excess) {
            self.encoded.remove(&key);
        }
    }
}

fn sixel_cache_key(
    frame: &DesiredFrame,
    placement: &GraphicPlacement,
    band: u16,
) -> Option<SixelCacheKey> {
    let asset = frame.assets.get(&placement.asset_key)?;
    let source_y = placement
        .source_y
        .saturating_add(u32::from(band).saturating_mul(frame.cell_pixels.height));
    let visible_height = frame
        .cell_pixels
        .height
        .min(asset.key.pixels.height.saturating_sub(source_y));
    let visible_width = u32::from(placement.clip.width)
        .saturating_mul(frame.cell_pixels.width)
        .min(asset.key.pixels.width);
    Some(SixelCacheKey {
        asset_key: placement.asset_key,
        source_y,
        visible_width,
        visible_height,
        cells: Size::new(placement.clip.width, 1),
    })
}

fn visible_image(
    asset: &RasterAsset,
    source_y: u32,
    visible_width: u32,
    visible_height: u32,
) -> io::Result<RgbaImage> {
    let width = asset.key.pixels.width;
    if width == 0 || visible_width == 0 || visible_height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "visible sixel raster has zero size",
        ));
    }
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel width overflow"))?;
    let start = usize::try_from(source_y)
        .ok()
        .and_then(|row| row.checked_mul(row_bytes))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel offset overflow"))?;
    let source_length = usize::try_from(visible_height)
        .ok()
        .and_then(|height| height.checked_mul(row_bytes))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel source size overflow"))?;
    let end = start
        .checked_add(source_length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel range overflow"))?;
    let source = asset.rgba.get(start..end).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "sixel raster slice is missing")
    })?;
    let visible_row_bytes = usize::try_from(visible_width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel clip overflow"))?;
    let rgba = source
        .chunks_exact(row_bytes)
        .flat_map(|row| row[..visible_row_bytes].iter().copied())
        .collect::<Vec<_>>();
    let image = RgbaImage::from_raw(visible_width, visible_height, rgba).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "sixel raster shape is invalid")
    })?;
    Ok(image)
}

fn write_upload<W: Write>(writer: &mut W, image_id: u32, asset: &RasterAsset) -> io::Result<()> {
    let chunks = asset.rgba.chunks(RAW_CHUNK_BYTES).collect::<Vec<_>>();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let encoded = STANDARD.encode(chunk);
        if index == 0 {
            write!(
                writer,
                "\x1b_Ga=t,f=32,s={},v={},i={image_id},q=2,m={more};{encoded}\x1b\\",
                asset.key.pixels.width, asset.key.pixels.height
            )?;
        } else {
            write!(writer, "\x1b_Gm={more};{encoded}\x1b\\")?;
        }
    }
    Ok(())
}

fn write_placement<W: Write>(
    writer: &mut W,
    image_id: u32,
    placement_id: u32,
    placement: &GraphicPlacement,
    frame: &DesiredFrame,
) -> io::Result<()> {
    // Ratatui tracks the cursor position it last wrote. Preserve that position
    // while addressing a Kitty placement so the next differential draw starts
    // from the cursor location Ratatui expects.
    write!(
        writer,
        "\x1b7\x1b[{};{}H",
        placement.clip.y.saturating_add(1),
        placement.clip.x.saturating_add(1)
    )?;
    let visible_pixels = u32::from(placement.clip.height)
        .saturating_mul(frame.cell_pixels.height)
        .min(
            placement
                .asset_key
                .pixels
                .height
                .saturating_sub(placement.source_y),
        );
    let visible_width = u32::from(placement.clip.width)
        .saturating_mul(frame.cell_pixels.width)
        .min(placement.asset_key.pixels.width);
    write!(
        writer,
        "\x1b_Ga=p,i={image_id},p={placement_id},y={},w={},h={visible_pixels},c={},r={},C=1,q=2\x1b\\\x1b8",
        placement.source_y, visible_width, placement.clip.width, placement.clip.height
    )
}

#[must_use]
pub(crate) fn kitty_environment_hint() -> bool {
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "WezTerm")
}

#[must_use]
fn sixel_environment_hint() -> bool {
    std::env::var_os("SIXEL").is_some()
}

fn inside_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux")
}

#[cfg(unix)]
fn stdin_ready(input: &impl std::os::fd::AsFd, timeout: Duration) -> io::Result<bool> {
    use nix::sys::select::{FdSet, select};
    use nix::sys::time::{TimeVal, TimeValLike};

    let mut read_fds = FdSet::new();
    read_fds.insert(input.as_fd());
    let milliseconds = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    let mut timeout = TimeVal::milliseconds(milliseconds);
    select(None, &mut read_fds, None, None, &mut timeout)
        .map(|ready| ready > 0)
        .map_err(|error| io::Error::from_raw_os_error(error as i32))
}

#[cfg(windows)]
fn stdin_ready(
    input: &impl std::os::windows::io::AsRawHandle,
    timeout: Duration,
) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    let handle = input.as_raw_handle().cast();
    // SAFETY: `handle` is borrowed from `input`, which remains alive throughout
    // the wait, and the function does not take ownership of it.
    match unsafe { WaitForSingleObject(handle, milliseconds) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{CellRect, DesiredFrame, PixelSize};

    fn evidence(
        graphics_query_authoritative: bool,
        kitty: bool,
        sixel: bool,
        kitty_hint: bool,
        sixel_hint: bool,
    ) -> CapabilityEvidence {
        CapabilityEvidence {
            graphics_query_authoritative,
            kitty,
            sixel,
            kitty_hint,
            sixel_hint,
            cell_pixels: None,
        }
    }

    #[test]
    fn capability_fixtures_select_kitty_then_sixel_then_text() {
        let selected = |evidence| {
            select_graphics_backend(GraphicsPreference::Auto, evidence, true)
                .expect("automatic selection should not fail")
                .backend
        };

        assert_eq!(
            selected(evidence(true, true, true, false, false)),
            GraphicsBackendKind::Kitty,
            "Kitty wins when both protocols are reported"
        );
        assert_eq!(
            selected(evidence(true, false, true, false, false)),
            GraphicsBackendKind::Sixel
        );
        assert_eq!(
            selected(evidence(false, false, false, false, false)),
            GraphicsBackendKind::Text,
            "no response and no hint must preserve review through text"
        );
        assert_eq!(
            selected(evidence(true, false, false, true, false)),
            GraphicsBackendKind::Text,
            "a capability response must overrule a stale environment hint"
        );
        assert_eq!(
            selected(evidence(true, false, false, false, false)),
            GraphicsBackendKind::Text
        );

        assert_eq!(
            select_graphics_backend(
                GraphicsPreference::Auto,
                evidence(true, true, true, false, false),
                false,
            )
            .expect("plain documents should not need a graphics backend")
            .backend,
            GraphicsBackendKind::Text,
            "no-graphics content stays on the text projection without starting a backend"
        );
    }

    #[test]
    fn no_response_uses_environment_hints_only_as_fallback_evidence() {
        let selection = select_graphics_backend(
            GraphicsPreference::Auto,
            evidence(false, false, false, true, false),
            true,
        )
        .expect("automatic selection should use a fallback hint");

        assert_eq!(selection.backend, GraphicsBackendKind::Kitty);
        assert_eq!(
            selection.reason.as_deref(),
            Some(
                "selected from an environment fallback hint after no authoritative graphics response"
            )
        );
    }

    #[test]
    fn non_authoritative_response_keeps_environment_hint_as_fallback_evidence() {
        let non_authoritative = evidence(false, false, false, true, false);

        assert_eq!(
            select_graphics_backend(GraphicsPreference::Auto, non_authoritative, true)
                .expect("an untested protocol may use a fallback hint")
                .backend,
            GraphicsBackendKind::Kitty
        );
    }

    #[test]
    fn unavailable_explicit_backend_returns_actionable_diagnostic() {
        let error = select_graphics_backend(
            GraphicsPreference::Sixel,
            evidence(true, true, false, false, true),
            true,
        )
        .expect_err("explicit sixel must not silently choose Kitty");

        assert!(error.contains("requested graphics backend 'sixel' is unavailable"));
        assert!(error.contains("--graphics auto"));
        assert!(error.contains("--graphics text"));
    }

    #[test]
    fn explicit_backend_is_honored_for_plain_documents() {
        let selection = select_graphics_backend(
            GraphicsPreference::Kitty,
            evidence(true, true, false, false, false),
            false,
        )
        .expect("an available explicit backend should be honored");

        assert_eq!(selection.backend, GraphicsBackendKind::Kitty);
    }

    #[test]
    fn unavailable_explicit_backend_is_rejected_for_plain_documents() {
        let error = select_graphics_backend(
            GraphicsPreference::Kitty,
            evidence(true, false, false, false, false),
            false,
        )
        .expect_err("plain content must not bypass explicit availability checks");

        assert!(error.contains("requested graphics backend 'kitty' is unavailable"));
    }

    fn asset(hash: u8, width: u32, height: u32) -> Arc<RasterAsset> {
        let key = RasterAssetKey {
            content_hash: [hash; 32],
            pixels: PixelSize { width, height },
            rasterizer_version: 1,
        };
        Arc::new(RasterAsset {
            key,
            rgba: Arc::from(vec![hash; (width * height * 4) as usize]),
        })
    }

    fn placement(asset: &RasterAsset, x: u16) -> GraphicPlacement {
        let rect = CellRect {
            x,
            y: 1,
            width: 2,
            height: 1,
        };
        GraphicPlacement {
            element_key: ElementKey(1),
            asset_key: asset.key,
            target: rect,
            clip: rect,
            source_y: 0,
            source: 10..20,
            source_regions: Vec::new(),
        }
    }

    fn frame(asset: Arc<RasterAsset>, placement: Option<GraphicPlacement>) -> DesiredFrame {
        let mut assets = HashMap::new();
        assets.insert(asset.key, asset);
        DesiredFrame {
            generation: 1,
            cell_pixels: PixelSize {
                width: 8,
                height: 16,
            },
            placements: placement.into_iter().collect(),
            assets,
        }
    }

    #[test]
    fn reconciliation_uploads_once_moves_in_place_and_reuses_scrolled_asset() {
        let image = asset(1, 16, 16);
        let mut backend = KittyBackend::new();
        let initial = frame(Arc::clone(&image), Some(placement(&image, 0)));
        let first = backend.reconcile(&initial);
        assert!(matches!(
            first.as_slice(),
            [KittyOperation::Upload { .. }, KittyOperation::Place { .. }]
        ));

        let mut moved_placement = placement(&image, 3);
        moved_placement.clip.x = 3;
        let moved = frame(Arc::clone(&image), Some(moved_placement));
        let second = backend.reconcile(&moved);
        assert!(matches!(second.as_slice(), [KittyOperation::Place { .. }]));

        let empty = frame(Arc::clone(&image), None);
        let third = backend.reconcile(&empty);
        assert!(matches!(
            third.as_slice(),
            [KittyOperation::DeletePlacement { .. }]
        ));

        let fourth = backend.reconcile(&initial);
        assert!(matches!(fourth.as_slice(), [KittyOperation::Place { .. }]));
    }

    #[test]
    fn asset_change_removes_old_placement_and_upload() {
        let first_asset = asset(1, 16, 16);
        let second_asset = asset(2, 32, 16);
        let mut backend = KittyBackend::new();
        backend.reconcile(&frame(
            Arc::clone(&first_asset),
            Some(placement(&first_asset, 0)),
        ));
        let operations = backend.reconcile(&frame(
            Arc::clone(&second_asset),
            Some(placement(&second_asset, 0)),
        ));

        assert!(
            operations
                .iter()
                .any(|operation| matches!(operation, KittyOperation::Upload { .. }))
        );
        assert!(
            operations
                .iter()
                .any(|operation| matches!(operation, KittyOperation::DeletePlacement { .. }))
        );
        assert!(
            operations
                .iter()
                .any(|operation| matches!(operation, KittyOperation::DeleteImage { .. }))
        );
    }

    #[test]
    fn encoder_chunks_uploads_and_suppresses_terminal_responses() {
        let image = asset(1, 64, 16);
        let mut output = Vec::new();
        write_upload(&mut output, 7, &image).expect("upload should encode");
        let encoded = String::from_utf8(output).expect("Kitty sequence is UTF-8");

        assert!(encoded.contains("a=t,f=32,s=64,v=16,i=7,q=2,m=1"));
        assert!(encoded.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn placement_preserves_the_text_renderer_cursor() {
        let image = asset(1, 16, 16);
        let mut clipped = placement(&image, 3);
        clipped.source_y = 8;
        let desired = frame(Arc::clone(&image), Some(clipped));
        let mut output = Vec::new();

        write_placement(
            &mut output,
            7,
            11,
            desired.placements.first().expect("placement should exist"),
            &desired,
        )
        .expect("placement should encode");
        let encoded = String::from_utf8(output).expect("placement is UTF-8");

        assert!(encoded.starts_with("\x1b7\x1b[2;4H"));
        assert!(encoded.contains("y=8,w=16,h=8"));
        assert!(encoded.ends_with("\x1b8"));
    }

    #[test]
    fn kitty_placement_crops_the_source_to_the_horizontal_clip() {
        let image = asset(1, 16, 16);
        let mut clipped = placement(&image, 0);
        clipped.clip.width = 1;
        let desired = frame(Arc::clone(&image), Some(clipped));
        let mut output = Vec::new();

        write_placement(
            &mut output,
            7,
            11,
            desired.placements.first().expect("placement should exist"),
            &desired,
        )
        .expect("placement should encode");
        let encoded = String::from_utf8(output).expect("placement is UTF-8");

        assert!(encoded.contains("y=0,w=8,h=16,c=1,r=1"));
        assert!(!encoded.contains("w=16,h=16,c=1"));
    }

    #[test]
    fn scrolling_reuses_the_upload_and_emits_only_one_small_replacement() {
        let image = asset(1, 16, 16);
        let mut backend = KittyBackend::new();
        backend
            .render(
                &frame(Arc::clone(&image), Some(placement(&image, 0))),
                &mut Vec::new(),
            )
            .expect("initial frame should render");

        let mut moved = placement(&image, 3);
        moved.clip.x = 3;
        let mut output = Vec::new();
        backend
            .render(&frame(image, Some(moved)), &mut output)
            .expect("scrolled frame should render");
        let encoded = String::from_utf8(output).expect("replacement is UTF-8");

        assert!(!encoded.contains("a=t"), "scrolling must not upload again");
        assert_eq!(encoded.matches("a=p").count(), 1);
        assert!(encoded.contains("a=p,i=1,p=1"));
        assert!(encoded.len() < 256, "scroll update should remain small");
    }

    #[test]
    fn failed_asset_replacement_keeps_old_and_new_uploads_for_cleanup() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected write failure"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let first_asset = asset(1, 16, 16);
        let second_asset = asset(2, 32, 16);
        let mut backend = KittyBackend::new();
        backend
            .render(
                &frame(Arc::clone(&first_asset), Some(placement(&first_asset, 0))),
                &mut Vec::new(),
            )
            .expect("initial frame should render");

        let error = backend
            .render(
                &frame(Arc::clone(&second_asset), Some(placement(&second_asset, 0))),
                &mut FailingWriter,
            )
            .expect_err("replacement should fail at the writer");
        assert_eq!(error.kind(), io::ErrorKind::Other);

        let mut cleanup = Vec::new();
        backend.clear(&mut cleanup).expect("cleanup should encode");
        let cleanup = String::from_utf8(cleanup).expect("Kitty cleanup is UTF-8");
        assert!(cleanup.contains("i=1"), "old upload must remain known");
        assert!(
            cleanup.contains("i=2"),
            "new upload may have reached the terminal"
        );
    }

    #[test]
    fn resize_clear_deletes_old_state_then_forces_a_complete_frame() {
        let image = asset(1, 16, 16);
        let desired = frame(Arc::clone(&image), Some(placement(&image, 0)));
        let mut backend = KittyBackend::new();
        backend.reconcile(&desired);
        let mut deletion = Vec::new();
        backend
            .clear(&mut deletion)
            .expect("resize clear should encode");
        let deletion = String::from_utf8(deletion).expect("Kitty deletion is UTF-8");

        assert!(deletion.contains("a=d,d=I,i=1"));
        assert!(matches!(
            backend.reconcile(&desired).as_slice(),
            [KittyOperation::Upload { .. }, KittyOperation::Place { .. }]
        ));
    }

    #[test]
    fn sixel_repaints_the_complete_scene_and_reuses_encoded_pixels() {
        let image = asset(1, 16, 16);
        let desired = frame(Arc::clone(&image), Some(placement(&image, 0)));
        let mut backend = SixelBackend::new();
        let mut first = Vec::new();
        backend
            .render(&desired, &mut first)
            .expect("initial sixel frame should render");
        let cache_size = backend.encoded.len();
        let mut second = Vec::new();
        backend
            .render(&desired, &mut second)
            .expect("unchanged sixel frame should repaint");

        assert_eq!(cache_size, 1);
        assert_eq!(backend.encoded.len(), 1);
        assert_eq!(first, second, "sixel must repaint the complete scene");
        assert!(first.windows(2).any(|bytes| bytes == b"\x1bP"));
    }

    #[test]
    fn sixel_clear_forces_a_full_alternate_screen_redraw_without_dropping_cache() {
        let image = asset(1, 16, 16);
        let desired = frame(Arc::clone(&image), Some(placement(&image, 0)));
        let mut backend = SixelBackend::new();
        backend
            .render(&desired, &mut Vec::new())
            .expect("initial sixel frame should render");
        let mut output = Vec::new();

        backend
            .clear(&mut output)
            .expect("sixel clear should encode");

        assert_eq!(output, b"\x1b[2J\x1b[H");
        assert_eq!(backend.encoded.len(), 1);
    }

    #[test]
    fn shared_desired_frame_keeps_protocol_differences_inside_backends() {
        let image = asset(1, 16, 16);
        let desired = frame(Arc::clone(&image), Some(placement(&image, 0)));
        let original_placements = desired.placements.clone();
        let mut kitty = KittyBackend::new();
        let mut sixel = SixelBackend::new();
        let mut kitty_output = Vec::new();
        let mut sixel_output = Vec::new();

        kitty
            .render(&desired, &mut kitty_output)
            .expect("Kitty should consume the shared frame");
        sixel
            .render(&desired, &mut sixel_output)
            .expect("sixel should consume the shared frame");

        assert_eq!(desired.placements, original_placements);
        assert!(kitty_output.windows(4).any(|bytes| bytes == b"a=t,"));
        assert!(sixel_output.windows(2).any(|bytes| bytes == b"\x1bP"));
        assert_ne!(kitty_output, sixel_output);
    }

    #[test]
    fn sixel_cache_evicts_old_scroll_slices_of_the_same_asset() {
        let image = asset(1, 16, 96);
        let mut backend = SixelBackend::new();

        for source_y in 0..70 {
            let mut visible = placement(&image, 0);
            visible.source_y = source_y;
            let mut desired = frame(Arc::clone(&image), Some(visible));
            desired.generation = u64::from(source_y) + 1;
            backend
                .render(&desired, &mut Vec::new())
                .expect("scroll slice should encode");
        }

        assert_eq!(backend.encoded.len(), MAX_ENCODED_SIXELS);
        assert!(backend.encoded.keys().all(|key| key.source_y >= 6));
    }

    #[test]
    fn sixel_scroll_reuses_overlapping_cell_bands() {
        let image = asset(1, 16, 80);
        let mut visible = placement(&image, 0);
        visible.target.height = 3;
        visible.clip.height = 3;
        let mut backend = SixelBackend::new();
        backend
            .render(
                &frame(Arc::clone(&image), Some(visible.clone())),
                &mut Vec::new(),
            )
            .expect("initial bands should encode");

        visible.source_y = 16;
        let mut scrolled = frame(image, Some(visible));
        scrolled.generation = 2;
        backend
            .render(&scrolled, &mut Vec::new())
            .expect("scrolled bands should encode");

        assert_eq!(backend.encoded.len(), 4);
        assert_eq!(
            backend
                .encoded
                .keys()
                .filter(|key| matches!(key.source_y, 16 | 32))
                .count(),
            2,
            "overlapping bands should be shared between adjacent frames"
        );
    }

    #[test]
    fn sixel_encoding_honors_the_horizontal_clip() {
        let image = asset(1, 16, 16);
        let mut clipped = placement(&image, 0);
        clipped.clip.width = 1;
        let desired = frame(Arc::clone(&image), Some(clipped));
        let mut backend = SixelBackend::new();

        backend
            .render(&desired, &mut Vec::new())
            .expect("clipped sixel should encode");

        let key = backend
            .encoded
            .keys()
            .next()
            .expect("encoded clip should be cached");
        assert_eq!(key.visible_width, desired.cell_pixels.width);
        assert_eq!(key.cells.width, 1);
    }
}
