# raden

Terminal markdown viewer that inlays pixel-perfect rendering — images, math,
tables — into plain terminal text, built around reviewing AI-written markdown.

Named after 螺鈿 (raden), the craft of inlaying luminous shell fragments into
lacquer: the body stays text, and only the parts that aid understanding shine.

## Overview

- A markdown viewer for the terminal. The text body is rendered as normal
  terminal text; understanding-critical elements are rendered pixel-perfect
  via terminal graphics protocols (sixel, kitty image protocol, etc.).
- Rendering targets: images, math (LaTeX), and tables are must-haves.
  Diagrams and similar elements are candidates.
- Designed around the workflow of reviewing AI-written markdown — not a
  general-purpose viewer. Inline comments on the document are a planned
  capability (design not finalized).
- Starts as a one-shot file viewer; a live/watch mode is intended later.
  The author expects the core architecture to be largely the same either way.

## Current viewer

Run the one-file viewer from the development shell:

```console
nix develop -c cargo run -- examples/showcase.md
```

The showcase covers headings, inline styles, nested and ordered lists, task
lists, quotations, code, tables, math events, a horizontal rule, HTML, and an
image diagnostic. You can also replace it with any local Markdown path.
Use `examples/showcase-ja.md` for a focused Japanese typography, wrapping, and
fallback check.

Local PNG, JPEG, WebP, and GIF images render automatically through Kitty
Graphics when raden runs in Kitty or WezTerm, and through sixel when its
experimental switch is enabled in a supported terminal. GIFs use their first
frame. Image paths are resolved
relative to the Markdown file's directory; remote URLs, absolute paths, paths
that escape that directory, unsupported formats, oversized images, and decode
failures stay readable as an inline diagnostic. Without a selected graphics
backend, the same elements use their alt-text projection.

Level-one and level-two headings also render as larger rasterized typography in
Kitty and WezTerm, or through the explicitly enabled experimental sixel
backend. They use an embedded Noto Sans JP font so Japanese and Latin text render
consistently; level-three through level-six headings remain terminal text.
Unsupported terminals and rasterization failures retain readable heading text.
The bundled font is licensed under the SIL Open Font License 1.1; see
`assets/fonts/OFL-NotoSansJP.txt`.

To use the same monospace or Nerd Font as the terminal, create
`$XDG_CONFIG_HOME/raden/config.toml` (or `~/.config/raden/config.toml` when
`XDG_CONFIG_HOME` is unset):

```toml
[heading]
families = ["JetBrainsMono Nerd Font Mono"]
```

Installed font families are tried in order, followed by the embedded Noto Sans
JP fallback. Unavailable families are skipped unless none of the configured
families can be resolved. Use a Nerd Font `Mono` family when its glyphs should
retain terminal-like spacing.

GFM tables render through an embedded Typst-to-SVG-to-RGBA pipeline in Kitty,
WezTerm, and the explicitly enabled experimental sixel backend. Their canvas is
transparent so the terminal background remains visible. Column widths fit
parsed cell content up to a deterministic cap and
only expand as far as needed, bounded by the viewport; wide cells wrap instead
of shrinking the text. Raden determines the terminal height from the original
table rows and their precomputed cell wrapping, with one extra row of breathing
room. That logical height is converted through the terminal's measured cell
pixels, and the SVG is scaled uniformly to it; width follows from the SVG aspect
ratio. If the
viewport cannot give every column a readable minimum width, or if
table compilation or rasterization fails, raden re-lays out every cell as
source-mapped terminal text. The renderer embeds its font and exposes no Typst
package, network, or filesystem resolver.

Tables use a booktabs-like paper style: no vertical grid or left/right border,
with strong top and bottom rules plus a lighter rule separating the bold header
from the body.

Sixel is available as an experimental second graphics backend. Capability
detection is not enabled yet, so select it explicitly in the same config file:

```toml
[graphics]
experimental_sixel = true
```

This is a temporary, sixel-only opt-in rather than the final backend override
contract. Automatic capability probing and fallback remain deferred. The footer
always labels the sixel backend `experimental`.

To keep hostile or accidental input bounded, a rasterized table may contain at
most 32 columns and 2,048 cells, occupy at most 256 terminal rows, and consume
at most 128 MiB. A layout rasterizes the four tables nearest the current source
anchor and allows at most 64 MiB of table pixels at once; other tables retain
their complete text projection until they approach the viewport. Exceeding a
per-table or layout limit also produces that readable projection with a concise
diagnostic instead of clipping or omitting source content.

The initial Reading mode uses these deliberately reversible bindings:

- `j` / `k` or arrow keys: scroll one rendered row
- `Ctrl-d` / `Ctrl-u`, Page Down / Page Up, or Space: scroll one page
- `g` / `G` or Home / End: jump to the start or end
- `q` or Escape: exit

The viewport is anchored to source byte positions rather than terminal cells,
so resizing reflows the document while keeping the same logical content in
view.

## Kitty graphics check

Use a Markdown fixture with level-one or level-two headings, or one that
references a local PNG, JPEG, WebP, or GIF, then run the viewer in Kitty or
WezTerm:

```console
nix develop -c cargo run -- /path/to/fixture.md
```

Check the backend label in the footer and verify this lifecycle:

1. The image appears at its Markdown position with its original aspect ratio.
2. Scroll it completely off screen and back; no stale placement remains.
3. Resize narrower and wider; the image is replaced at the new size and the
   same source content remains near the viewport anchor.
4. Add missing, remote, unsupported, and corrupt image references; each becomes
   readable diagnostic text while valid images continue rendering.
5. Quit with `q`, then confirm the shell has no image placement left behind.
6. Verify H1 and H2 are visually distinct, Japanese text is intact, and narrow
   terminal widths wrap headings without clipping them.
7. Inspect the tables in both showcase files: headers, alignment, borders,
   Unicode, and wrapped cells remain legible; narrowing below the readable
   column minimum switches to text without dropping a cell.

The automated tests separately cover desired-frame reconciliation, upload and
placement replacement, stale deletion, cache ownership, path escape rejection,
decode fallback, table layout and overflow, Unicode, cache keys, failure
projection, and source ranges. Real-terminal checks remain necessary because
terminal graphics behavior cannot be observed through a test backend.

## Experimental sixel graphics check

Enable the experimental sixel switch above and run the same representative Markdown
fixture in a sixel terminal, such as xterm with sixel enabled, foot, or mlterm.
Do not enable this switch in a terminal without sixel support because the
capability probe and automatic fallback are intentionally deferred.

Verify the initial render and the same scroll, resize, invalid-image, and quit
lifecycle listed for Kitty. In addition, confirm that every changed frame
clears the previous graphics occupancy and repaints the complete visible scene,
with no stale fragments after scrolling away and back. A brief full-screen
redraw is expected: portable partial sixel deletion is not reliable across the
target terminals.

## Motivation

- Reviewing AI-written markdown happens in the terminal, but web rendering is
  easier to read, so the author keeps switching to VS Code, Zed, or a browser.
- Projects like mdroll and glowm suggest that sixel / kitty image protocol can
  close most of that readability gap without leaving the CLI.

## Notes

- Related projects:
  - [mdroll](https://github.com/tokuhirom/mdroll), [glowm](https://github.com/atani/glowm) — inspiration for the approach.
  - [mdfried](https://github.com/benjajaja/mdfried) — very close prior art:
    Rust, images via sixel/kitty/iTerm2 (chafa fallback), image slicing for
    natural scrolling, big headers, syntax highlighting, mermaid, pager/search.
    Its image layer is extracted as
    [ratatui-image](https://github.com/benjajaja/ratatui-image).
  - Gaps observed in mdfried (as of 2026-08): live reload, math rendering,
    and any review-oriented workflow.
- Differentiation: build for the AI-review use case — inline comments, math
  and tables as first-class citizens — rather than competing as a
  general-purpose viewer.
- Not finalized: automatic graphics capability selection, inline comment
  design, and live mode design.
- Naming: rejected candidates — makie (collides with Makie.jl), galley
  (collides with an AI-coding CLI and a macOS PDF previewer), inlay ("inlay
  hints" association), tipin (obscure), focal (generic, Ubuntu Focal Fossa).
  raden pairs with the author's urushi project (lacquer-craft lineage).
