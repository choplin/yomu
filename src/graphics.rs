use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::{DesiredFrame, ElementKey, GraphicPlacement, RasterAsset, RasterAssetKey};

const RAW_CHUNK_BYTES: usize = 3_072;
const MAX_UPLOADED_IMAGES: usize = 64;

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
    write!(
        writer,
        "\x1b_Ga=p,i={image_id},p={placement_id},y={},w={},h={visible_pixels},c={},r={},C=1,q=2\x1b\\\x1b8",
        placement.source_y,
        placement.asset_key.pixels.width,
        placement.clip.width,
        placement.clip.height
    )
}

#[must_use]
pub(crate) fn kitty_environment_hint() -> bool {
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|term| term.contains("kitty"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "WezTerm")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{CellRect, DesiredFrame, PixelSize};

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
}
