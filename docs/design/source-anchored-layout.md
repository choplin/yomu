# Source-Anchored Layout

This document answers how Raden preserves logical reading position and source
identity while wrapping, resizing, or replacing rich graphics with text.

## Decision

Half-open UTF-8 byte ranges in the original Markdown source are the canonical
identity for document content. Display rows, terminal cells, and line/column
coordinates are derived projections and must not become an independent source
of truth.

`Document::parse` retains the ranges produced by
`pulldown_cmark::Parser::into_offset_iter`. The document builder assigns ranges
to semantic elements and to synthetic projections such as list markers. During
layout, every `LayoutFragment` and `LayoutRow` retains the source it represents.
Raster placements retain both an element-level source range and, where useful,
subregions such as table cells.

`App` stores the viewport as the first visible row's source range plus a cell
offset within that row. To interpret it against a layout:

1. prefer the row with the same complete range;
2. otherwise choose the row containing the saved start offset;
3. otherwise choose the first row at or after that offset; and
4. clamp the intra-row offset to the selected row's current height.

Scrolling is expressed in terminal-cell heights, but committing a new position
immediately converts it back to this source-based form. After a width change or
graphics-to-text re-layout, the new row geometry is therefore derived from the
saved source position rather than from the old row number.

## Why byte ranges are canonical

Pulldown-cmark already reports byte-accurate ranges into the UTF-8 input.
Keeping that coordinate through parsing and layout gives prose and rich
elements one identity without reconstructing source positions after wrapping.
It also matches Rust string slicing and allows synthetic display content to
point at the Markdown construct that caused it.

Rendered rows are unsuitable as canonical identity because their count changes
with viewport width, font cell geometry, raster availability, and text
fallback. Line and column coordinates would introduce another conversion with
ambiguous Unicode width behavior. The source range remains stable for the
lifetime of the parsed `Document`; Raden does not claim that it survives edits
to the source file.

## Mapping rules

Text wrapping maps each output fragment proportionally through the source range
that produced its input text. Synthetic fragments map to their triggering
source position. Specialized elements keep their enclosing range rather than
inventing ranges from rendered pixels.

A table raster additionally carries `GraphicSourceRegion` values for its cells.
Hit-testing a position inside a known region returns that cell's source range;
positions outside those regions return the table's enclosing range. Cropping a
graphic to the viewport crops these regions in cell coordinates without
changing their source ranges.

An inline graphic participates in its logical text row. A block graphic owns a
row whose `height` is its terminal-cell height. `top_row_offset` identifies
which cell-height band of that row is first visible, so tall graphics can be
scrolled one terminal row at a time without losing the element's source anchor.

## Re-layout and asynchronous rendering

The application always has a synchronous text layout. A graphics-enhanced
layout may be produced on the logical-renderer thread. Each request includes the
current generation, source anchor, body rectangle, and cell-pixel size.

The worker keeps only the latest pending request. Before publishing a result it
checks whether a newer request is waiting, and the application accepts a
published result only if its generation equals the current generation. When
the enhanced layout arrives, the application resolves the saved source anchor
against it and derives a new desired frame. This prevents an earlier resize or
scroll computation from moving the current viewport backward.

Rasterizers do not provide cooperative cancellation. Dropping the renderer
therefore detaches in-flight work instead of delaying quit; generation checks
prevent that abandoned result from entering application state.

## Rejected alternatives

### Store the rendered row index

A row index is simple during steady-state scrolling, but it identifies different
content after wrapping or projection heights change. Restoring it after resize
would preserve screen geometry at the cost of losing the reader's place.

### Store terminal line and column

Terminal coordinates combine source identity with Unicode display width and the
current wrapping decision. They would require reverse mapping after every
layout and still could not identify a whole table, image, or multi-row formula
without additional state.

### Force raster and text projections into equal rectangles

Equal rectangles would make row indices appear stable, but text tables,
diagnostics, and formulas need different heights from their rasters. Clipping
them to the old rectangle would discard reviewable content. Raden instead
rebuilds layout and preserves the source anchor.

### Apply every completed background render

Background work can finish out of order relative to resize and input. Applying
every result would let stale geometry replace current state. Latest-pending
admission plus generation validation makes freshness explicit without coupling
rasterizers to cancellation.
