# Graphics Selection and Fallback

This document answers how Raden uses Urushi terminal graphics without making
document reviewability depend on a graphics protocol or successful
rasterization.

## Decision

Every rich element has a cell-aligned RGBA projection and a readable text
projection. Raden embeds prepared RGBA in `urushi_graphics::Image`, composes it
with `ImagePresentation`, and supplies the text projection as the component's
fallback. The resulting Image is an ordinary part of the application `View`.

Raden owns raster preparation and application geometry. `urushi-tui-app` with
its `graphics` feature owns capability observation, protocol selection, View
resolution, clipping, Kitty/Sixel lifecycle state, transactional frame commit,
failure recovery, resize handling, and shutdown cleanup.

## Selection

The runtime accepts `auto`, `kitty`, `sixel`, or `text`. Automatic selection
prefers positively confirmed Kitty support, then Sixel when uniform cell-pixel
geometry is known, and finally text. Explicit Kitty or Sixel is a requirement;
an unavailable explicit protocol fails startup.

Explicit text mode skips Raden raster effects. In automatic mode Raden may
prepare RGBA before learning that the runtime selected text; this is bounded
extra work and preserves the separation between application state and terminal
capabilities.

## Element-level degradation

Failure to prepare one element does not disable other images. The layout uses
that element's text projection and an actionable reason where available. This
covers rejected paths, unavailable or unsupported files, decode and conversion
failures, glyph limitations, viewport constraints, and memory budgets.

Local image access remains confined to regular files beneath the Markdown
file's canonical directory. Table and math rasterization use a data-only Typst
pipeline with embedded fonts and no package, network, or filesystem resolver.
Complexity, dimensions, and cache bytes are bounded.

## Protocol-level degradation

Urushi resolves fallback cells and image anchors from the same View. Kitty
retains uploads and placements; Sixel repaints the complete visible image scene.
Their state is internal to `urushi-graphics` and the runtime presenter.

On a graphics-output failure, the Urushi presenter attempts protocol cleanup,
disables graphics for the remaining session, redraws the same View's text
fallback, and publishes the error through the terminal-error subscription.
Raden records the diagnostic in its footer but does not operate the backend.
Resize and shutdown use the same Urushi-owned lifecycle.

## Asynchronous preparation

Raden returns raster preparation as `Effect::perform_latest` under one stable
key. Its completion carries prepared assets, not a completed `Layout`; `view`
derives layout from the current model and those assets. Replacement suppresses
obsolete queued work at the runtime boundary, and a generation check also
rejects work that completed after a newer scroll or resize. The shared
`RasterCache` preserves expensive RGBA results across effects without
introducing an application-owned executor or worker channel.

## Cache boundaries

`RasterCache` owns decoded and generated RGBA reuse. Urushi owns Kitty upload
state and Sixel encoded-band reuse. Losing a protocol cache can cost output work
but cannot change Markdown meaning, source navigation, or fallback content.

## Rejected alternatives

### Keep a Raden graphics backend

A second backend would duplicate capability selection, resize ordering,
transactional commit, protocol recovery, and cleanup already provided by
Urushi, and would require a side channel from `view` to terminal flush.

### Keep a parallel desired-scene model

Urushi Image anchors already derive placement and clipping from the resolved
View. A separate `DesiredFrame` would create a second geometric truth and make
cell and graphics commits diverge.

### Rasterize inside `update`

Image decoding, font rendering, and Typst compilation are blocking work.
Running them in `update` would stall ordered input delivery. They remain
runtime-managed blocking effects.

### Leave a blank rectangle after failure

Blank space removes source content. Every Image component therefore carries a
readable fallback, including headings, math, tables, and local images.
