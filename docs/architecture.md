# Architecture

This document is the starting point for developers changing Yomu. It explains
how Markdown source becomes a source-aware Urushi view and which responsibilities
remain application-specific.

Yomu is one Rust package with a library and CLI binary. The top-level
[`README`](../README.md) describes user-visible behavior. Precise rules for
Theme ownership and resolution live in [`theme.md`](theme.md). Precise rules for
source identity and graphics degradation live in
[`design/source-anchored-layout.md`](design/source-anchored-layout.md) and
[`design/graphics-fallback.md`](design/graphics-fallback.md). The history of the
design decisions those documents record lives in
[`decision-log.md`](decision-log.md).

## Architecture at a glance

```text
file or stdin
     |
     v
Document: semantic elements + UTF-8 source spans
     |
     +---- Theme: resolved app/document roles + raster policy
     |
     +-----------------------+
     |                       |
     v                       v
text projection         raster preparation
     |                  image / heading / table / math
     +-----------+-----------+
                 v
        Layout: rows + source identity
                 |
                 v
        Urushi View + Image components
                 |
                 v
 urushi-tui-app graphics presentation
       cells + Kitty/Sixel lifecycle
```

Yomu owns Markdown meaning, source-aware navigation, raster preparation,
application-semantic Theme roles, and application state. Urushi owns the
terminal session, background observation and light/dark classification, event delivery, effects,
view resolution, cell-frame commit, graphics protocol selection and lifecycle,
failure recovery, and cleanup.

## Core representations and ownership

### Source document

[`src/document.rs`](../src/document.rs) parses Markdown and retains each
pulldown-cmark event's half-open UTF-8 source range. Its ordered segments and
specialized collections preserve the data needed for images, headings, tables,
math, links, and code blocks. It does not inspect terminal capabilities or
produce terminal commands.

### Projection and layout

[`src/layout.rs`](../src/layout.rs) owns width-dependent projection into
terminal rows. It wraps text, maps fragments back to source spans, and
reserves cell geometry for prepared raster elements. `Layout` is a transient
View-construction value: `ReaderModel` never stores one. Each call to `view`
derives the current layout from the document, surface, navigation state, and
the prepared raster assets currently in the model.

Every rich element retains a readable fallback. Images keep labels and
diagnostics, headings keep their text, tables keep their cells, and math keeps
its source notation. Raster and text projections may have different heights,
so every derived layout resolves the viewport from its source anchor.

### Theme resolution

[`src/theme.rs`](../src/theme.rs) owns Yomu's serializable partial Theme
schema, built-in Themes, single inheritance, recursive merge, validation, and
the complete `Theme` consumed by rendering. App roles and Markdown
roles remain Yomu concepts. Terminal colors lower to Urushi `TextStyle`, while
heading, table, math, and image raster parameters remain typed Yomu data.
Resolved rich-element styles are element-specific sum types: text or
rasterized content with an explicit readable fallback. The partial merge model
does not leak a presentation flag or an inactive render branch into runtime.
See [`theme.md`](theme.md) for the complete Theme ownership and resolution
model.

[`src/config.rs`](../src/config.rs) discovers user Themes next to `config.toml`
and applies config overrides after the selected Theme. Automatic light/dark
selection queries the terminal once through Urushi and uses Urushi's
`ThemeMode` classification; Yomu does not duplicate the query or luminance
policy. Graphics capability, backend selection, viewport width, cache capacity,
and safety limits are outside Theme.

### Raster preparation

[`src/image.rs`](../src/image.rs) owns local-image admission and the shared,
byte-bounded `RasterCache`. It confines paths to the Markdown directory,
validates formats and resource limits, and prepares cell-aligned RGBA. The same
cache coordinates:

- [`src/heading.rs`](../src/heading.rs) for rasterized heading typography;
- [`src/table.rs`](../src/table.rs) for bounded Typst table rendering;
- [`src/math.rs`](../src/math.rs) for TeX-to-Typst math rendering; and
- ordinary local images decoded by `image`.

Raster identities include every input that changes pixels and the resolved
Theme identity. Protocol upload and
encoded-band caches are not Yomu state; they are owned by `urushi-graphics`.

### Reading state and View construction

[`src/app.rs`](../src/app.rs) owns source-anchored viewport navigation. A
position is a source span plus an offset inside a multi-row element rather than
an absolute rendered row, so it survives resize and asynchronous raster
preparation.

[`src/ui.rs`](../src/ui.rs) converts the layout into one Urushi `View`. Text and
`urushi_graphics::Image` components share the same row geometry. An Urushi
vertical `Viewport` performs clipping, including the source crop of a partially
visible image. Yomu emits no Kitty or Sixel commands.

### Application and runtime

[`src/terminal.rs`](../src/terminal.rs) defines Yomu's
`urushi_tui_app::Application`: `ReaderModel`, `Message`, `init`, `update`,
`view`, and `subscriptions`. These are application-domain TEA values. The
runtime owns their execution.

Surface and input observations arrive through subscriptions. Expensive raster
work is returned as `Effect::perform_latest`; it runs outside the update thread,
and its generation is checked before its prepared assets may enter the model.
Layout remains a projection performed while building the View. The application
does not own a worker thread, channel, event loop, draw scheduler, terminal
session, or presentation side channel.

The production entry point opens Urushi's native controlling-terminal backend
and passes it to `Runtime`. With the `graphics` feature enabled,
`Runtime::graphics` selects text, Kitty, or Sixel from the requested preference
and observed terminal evidence. `urushi-tui-app` resolves the application View
once and commits its cell frame and image scene together.

## Representative flows

### Open and initial render

1. The CLI resolves file or standard-input Markdown plus optional graphics and
   Theme selections.
2. Yomu parses the document, loads configuration, resolves one validated
   Theme, and loads local-image metadata.
3. Urushi enters the terminal session, observes geometry and capabilities, and
   selects the graphics presentation.
4. The initial `Surface` message causes Yomu to return a latest-only raster
   effect when graphics have not been explicitly disabled.
5. The effect completion adds prepared assets to the model. `view` derives a
   layout containing Urushi Image components, and Urushi commits cells and
   graphics from that same resolved View.

### Scroll and resize

A key becomes a Yomu `Command`, and `App` advances through the currently
derived layout's cell heights while retaining only a source anchor in the
model. The next `view` derives layout again from that updated state. Yomu also
returns a latest-only effect so near-viewport raster preparation follows the
new focus.

On resize, Yomu clears assets tied to the old geometry and requests new
preparation. The next `view` derives the new-width text projection until those
assets arrive. Urushi owns presentation invalidation, graphics clearing, and
redraw against the accepted surface snapshot. A stale effect generation cannot
enter the model.

### Graphics failure

Element preparation failures select that element's text projection. Protocol
selection, writes, recovery, and cleanup are Urushi responsibilities. A graphics
write failure disables graphics in the Urushi presenter, redraws the same View's
text fallbacks, and reports the diagnostic through the terminal-error
subscription. An explicitly requested unavailable protocol fails startup.

## Binding invariants

- Source byte ranges, not display rows, are stable across wrapping and resize.
- Every rasterized element supplies a readable Urushi Image fallback.
- `renderer = "text"` prevents preparation for that element; other modes
  never weaken its readable fallback.
- Rendering consumes one validated `Theme`; override configuration does
  not reach layout, UI, or rasterizers.
- `Layout` owns application geometry; Urushi owns View clipping and physical
  presentation.
- Raster assets are aligned to terminal-cell pixels before becoming Image
  components.
- `ReaderModel` stores application state and prepared assets, never a `Layout`.
- Raster work and caches are bounded, and only the current generation may add
  prepared assets to the model.
- `update` requests expensive work through effects and performs no terminal I/O.
- Kitty/Sixel selection, state, commit, fallback, resize, and cleanup remain one
  `urushi-tui-app` presentation lifecycle.

## Where changes begin

Markdown meaning begins in `document.rs`, then follows the affected segment
through `layout.rs`. Raster changes begin in `image.rs` and the specialized
rasterizer. Navigation belongs in `app.rs`; message handling and effects belong
in `terminal.rs`; View composition belongs in `ui.rs`.

Graphics protocol behavior belongs in the Urushi workspace, not Yomu. A new
rasterized Yomu element needs bounded preparation, stable asset and placement
keys, a readable fallback, and an `ImagePresentation` in the View.

Release packaging and automation are described separately in
[`release-automation.md`](release-automation.md).
