# Graphics Selection and Fallback

This document answers how Raden uses terminal graphics without making document
reviewability depend on a graphics protocol or a successful rasterization.

## Decision

Every rich Markdown element has two possible projections:

- a backend-neutral RGBA asset placed in terminal-cell geometry; and
- a complete terminal-text representation that retains source identity.

`Layout` chooses the projection before it assigns rows. Text is therefore a
layout mode, not a graphics backend command and not content painted into a
raster-sized placeholder.

The backend-neutral `DesiredFrame` contains the complete graphics output wanted
for one generation: cell-pixel size, ordered placements, clipping and source
coordinates, and the assets those placements reference. It has no Kitty image
IDs, Kitty placement IDs, sixel palettes, or escape sequences.

Protocol state belongs to the selected backend:

- Kitty caches uploads by `RasterAssetKey`, reconciles stable element
  placements, and removes stale placements and unreferenced uploads.
- Sixel caches encoded visible bands, but retains no placement state. Every
  changed generation clears the alternate screen, lets Urushi redraw all text,
  and then paints the complete desired sixel scene.
- Text carries no protocol state. The document is laid out entirely from text
  projections and the desired graphics frame is empty.

## Selection

`auto` queries terminal capabilities only when the document needs graphics. It
selects Kitty first, then sixel, then text. A terminal response is authoritative
capability evidence; the selection logic does not infer support merely from a
terminal name.

An explicit `kitty` or `sixel` preference is a requirement. If the reported
capabilities do not support it, startup fails with a diagnostic suggesting
`auto` or `text`. An explicit choice must not silently change the user's test or
operating environment.

## Element-level degradation

Failure to prepare one element does not disable other graphics. The element is
replaced in layout by its own text projection and a concise reason when one is
available. This covers path rejection, unavailable files, unsupported formats,
decode and conversion failures, glyph limitations, viewport constraints, and
memory budgets.

Local image access is deliberately confined. A destination must resolve to a
regular file under the Markdown file's canonical parent directory. Remote and
data URLs, absolute paths, upward escapes including escapes through symlinks,
unsupported formats, excessive dimensions, and excessive decoded size are
rejected before they can become a raster asset.

Tables and math use an embedded, data-only Typst pipeline. Raden supplies the
source and fonts; no Typst package, network, or filesystem resolver is exposed.
Table complexity, raster dimensions, per-layout raster bytes, and shared cache
bytes are bounded. Crossing a bound selects text instead of truncating source
content.

## Backend-level degradation

A protocol write or flush failure is a boundary failure rather than an
element-level failure. The application:

1. records which graphics backend failed and exposes the reason in the footer;
2. discards pending graphics frames and requests backend disablement;
3. clears Kitty state when possible;
4. drops the logical renderer and its raster cache ownership;
5. rebuilds the complete text layout at the current source anchor; and
6. continues the terminal session with an empty desired graphics frame.

There is no automatic promotion to another graphics protocol mid-session. A
full text projection is the recovery boundary because it removes both protocol
state and asynchronous raster work from the failing path.

Resize and normal shutdown also clear protocol state. Kitty deletes known
terminal images. Sixel clears the alternate screen because it has no portable
partial-delete operation. Cleanup occurs inside the same terminal backend that
owns flush and session exit, preventing a second writer from inventing an
independent graphics lifecycle.

## Cache boundaries

`RasterCache` owns decoded or generated RGBA reuse and is bounded by byte size.
Its identities include the content and all rendering inputs that can change the
pixels. Cached failures prevent repeated expensive attempts for an unchanged
identity.

Kitty upload state and sixel encoded-band state are separate caches because
they answer protocol-specific questions. Neither cache is visible to
`Document`, `App`, or the layout builder. Losing a protocol cache may cost work,
but cannot change Markdown meaning, source mapping, or the availability of text
fallback.

## Rejected alternatives

### Model every backend as Kitty operations

Upload, place, move, and delete fit Kitty's retained images but not sixel's
repaint model. A Kitty-shaped common interface would expose one protocol's
lifecycle as application architecture.

### Use only lowest-common-denominator repainting

Forcing every backend to resend every visible raster would discard Kitty's
stable upload and placement identities. The desired scene is shared; diffing or
repainting it is backend-owned.

### Leave a blank rectangle after failure

Blank space removes the very source content Raden exists to help review and
makes viewport/source correspondence harder to understand. A readable text
projection is required even when its height changes.

### Fall through from an explicit backend request

Silent fallback would make capability tests and user intent ambiguous. Only
`auto` selects among alternatives; explicit modes either run or report why they
cannot.

### Keep layout and switch only the renderer

Raster and text projections have different geometry. Reusing the old rows would
clip, overlap, or omit text. Backend failure therefore triggers complete text
re-layout at the stable source anchor.
