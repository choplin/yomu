# Architecture

This document is the starting point for developers changing Raden. It explains
how Markdown source becomes a source-aware terminal view, where text and raster
rendering separate, how the terminal runtime coordinates them, and which
invariants changes must preserve.

Raden is a single Rust package with both a library and a CLI binary. The
top-level [`README`](../README.md) explains the product and its user-visible
behavior. This document describes the implementation that currently exists.
Precise rules for source identity and graphics degradation live in
[`design/source-anchored-layout.md`](design/source-anchored-layout.md) and
[`design/graphics-fallback.md`](design/graphics-fallback.md). Decision history
lives in [`decision-log.md`](decision-log.md).

## Architecture at a glance

Raden keeps ordinary text in the terminal's cell renderer and overlays only
understanding-critical elements as terminal graphics:

```text
file or stdin
     |
     v
Document: semantic elements + UTF-8 source spans
     |
     +-----------------------+
     |                       |
     v                       v
text projection         raster preparation
     |                  image / heading / table / math
     +-----------+-----------+
                 v
        Layout: terminal rows + source identity
                 |
          +------+------+
          |             |
          v             v
     Urushi View    DesiredFrame
     text cells     raster assets + placements
          |             |
          +------+------+
                 v
        one terminal presentation
```

The split is intentional. `Layout` is the shared geometric truth for both text
and graphics. `DesiredFrame` carries what graphics should be visible, without
encoding how Kitty or sixel realizes it. The terminal backend combines the
Urushi cell frame and Raden's graphics overlay at flush time.

## Core representations and ownership

### Source document

[`src/document.rs`](../src/document.rs) owns the semantic interpretation of
Markdown. `Document::parse` enables the supported pulldown-cmark extensions and
retains every parser event's half-open UTF-8 byte range. Its `Segment` sequence
is the ordered input to layout; specialized collections retain the data needed
for images, H1/H2 headings, tables, math, links, and code blocks.

`Document` does not resolve image paths, choose a terminal backend, measure a
viewport, or create terminal commands. Its lasting responsibility is to keep
renderable meaning attached to source identity.

### Projection and layout

[`src/layout.rs`](../src/layout.rs) owns width-dependent projection into
terminal rows. It wraps styled text, maps each fragment back to a source span,
and reserves cell rectangles for raster elements. The same builder can produce
a complete text layout or a graphics-enhanced layout.

Every rich element therefore has a readable text outcome. Images have labels
and diagnostics, math retains its delimiters, tables retain every cell, and
headings retain their text. Raster generation may change row height, so a
graphics-to-text transition rebuilds the layout rather than substituting text
inside an old rectangle. The exact contract is in
[`design/graphics-fallback.md`](design/graphics-fallback.md).

`Layout::desired_frame` derives only the visible graphics scene. It clips each
placement to the viewport, retains its full target rectangle and source range,
and supplies the raster assets referenced by that frame.

### Raster preparation

[`src/image.rs`](../src/image.rs) owns local-image admission and the shared
`RasterCache`. It confines image paths to the Markdown file's directory,
validates formats and resource bounds, decodes accepted files, and scales RGBA
assets to the terminal cell grid. The cache also coordinates the specialized
rasterizers:

- [`src/heading.rs`](../src/heading.rs) selects configured fonts with the
  embedded Noto Sans JP fallback and rasterizes H1/H2 typography;
- [`src/table.rs`](../src/table.rs) converts bounded, structured table data to
  Typst, then SVG and RGBA;
- [`src/math.rs`](../src/math.rs) converts supported TeX through Typst, then SVG
  and RGBA; and
- [`src/code.rs`](../src/code.rs) remains on the text path and adds syntax
  colors to source-mapped code fragments.

Raster identity includes the inputs that affect pixels, including content,
target geometry, fonts or render settings where applicable, and a rasterizer
version. The shared cache owns RGBA reuse and a byte-bounded LRU policy;
protocol-specific caches remain inside their graphics backends.

### Reading state and cell UI

[`src/app.rs`](../src/app.rs) owns Reading-mode commands and viewport state.
The canonical viewport position is a source span plus an offset within a
possibly multi-row element, not an absolute rendered row. This lets
[`src/terminal.rs`](../src/terminal.rs) replace the layout after resize or
graphics degradation while keeping the same logical content near the top.

[`src/ui.rs`](../src/ui.rs) projects the current layout into an Urushi `View`.
It owns the header, centered body width, footer, text styles, and the visible
cell rows. It does not emit Kitty or sixel commands.

### Runtime and terminal ownership

[`src/terminal.rs`](../src/terminal.rs) is the integration boundary. It owns the
`urushi-tui-app` application, opens the native controlling terminal, selects a
graphics backend from observed capabilities and configuration, and coordinates
cell drawing with graphics output.

Text layouts are available synchronously. When graphics are enabled, a
dedicated `LogicalRenderer` thread owns the raster cache and computes enhanced
layouts. Its mailbox retains only the latest request, and a completed render is
accepted only when its generation matches the current model. Expensive stale
work may finish, but it cannot replace a newer viewport or surface state.

`DirectGraphicsBackend` is the single presentation boundary. Urushi owns the
terminal session, events, cell drawing, and restoration through that backend;
Raden intercepts resize and flush boundaries to clear or present graphics in
the same terminal lifecycle.

### Graphics backends

[`src/scene.rs`](../src/scene.rs) defines the backend-neutral desired scene:
cell rectangles, stable element and raster identities, source mappings, and the
assets needed by one frame. [`src/graphics.rs`](../src/graphics.rs) owns backend
selection and protocol mechanics.

Kitty is retained-mode: it caches uploaded assets, reconciles desired
placements, reuses uploads, and deletes stale placements and unused images.
Sixel is immediate-mode: it caches encoded cell-height bands, but clears and
repaints the complete visible scene because portable partial deletion is not a
reliable contract. Text is the no-graphics projection, not a fake graphics
protocol.

## Representative flows

### Open and initial render

1. [`src/main.rs`](../src/main.rs) resolves a file or standard input and an
   optional graphics override.
2. [`src/lib.rs`](../src/lib.rs) parses the source and loads configuration.
3. The terminal backend enters its managed session, obtains window and
   capability evidence, and chooses Kitty, sixel, or text.
4. The application immediately creates a text layout. If graphics are active,
   it also submits the current source anchor and surface geometry to the logical
   renderer.
5. Urushi draws the text view; the terminal backend overlays the matching
   `DesiredFrame` during flush.

### Scroll and resize

A key becomes a `Command`, and `App` advances through rendered cell heights
while storing the resulting source anchor. Scrolling changes the desired
visible placements without reparsing the document.

On resize, the terminal backend first invalidates graphics occupancy. The
application rebuilds a text layout at the new width, clamps the source-anchored
viewport, increments the generation, and requests a new enhanced layout. A
stale generation is ignored. The detailed positioning rule is in
[`design/source-anchored-layout.md`](design/source-anchored-layout.md).

### Graphics failure

An element-level decode, conversion, rasterization, or budget failure produces
that element's text projection and a concise diagnostic. A backend write
failure disables the graphics mode, clears what can be cleared, rebuilds the
whole document through the text path at the current source anchor, and keeps
the terminal session alive. An unavailable explicitly requested backend fails
startup instead of silently selecting another backend.

## Binding invariants

- Source byte ranges, not display rows, are the stable identity across wrapping,
  resize, and graphics changes.
- A complete readable text projection exists independently of terminal graphics
  support and rasterization success.
- `Layout` owns cell geometry; protocol backends consume `DesiredFrame` and do
  not decide document layout.
- Raster assets are aligned to terminal-cell pixels before placement, so the
  protocol does not silently rescale a logically sized element.
- Capability evidence is observed before automatic backend selection; the
  order is Kitty, sixel, then text.
- Raster work and caches are bounded. Elements outside the near-viewport raster
  set or beyond resource limits remain complete text rather than disappearing.
- Only the current render generation may replace the application layout.
- Terminal graphics are cleared on resize, degradation, and session cleanup so
  stale placements do not survive their owning frame.

## Where changes begin

Changes to Markdown meaning begin in `document.rs`, then follow the affected
segment through `layout.rs` and its source-mapping tests. A new rasterized
element additionally needs a bounded raster plan, cache identity and ownership
in `image.rs`, a text projection, scene placement, and failure coverage.

Changes to navigation begin in `app.rs`; terminal event wiring belongs in
`terminal.rs`, and visible chrome belongs in `ui.rs`. Changes to backend
selection or protocol lifecycle begin in `graphics.rs` and must be integrated
through `DirectGraphicsBackend` so resize, flush, fallback, and cleanup remain
one lifecycle.

Tests live beside the owning modules. Pure parsing, layout, cache, scene, and
state behavior is covered there; actual terminal-protocol behavior still
requires the real-terminal checks documented in the top-level README.
