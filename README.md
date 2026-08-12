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
lists, quotations, code, tables, math events, a horizontal rule, HTML, and the
current image fallback. You can also replace it with any local Markdown path.

The initial Reading mode uses these deliberately reversible bindings:

- `j` / `k` or arrow keys: scroll one rendered row
- `Ctrl-d` / `Ctrl-u`, Page Down / Page Up, or Space: scroll one page
- `g` / `G` or Home / End: jump to the start or end
- `q` or Escape: exit

The viewport is anchored to source byte positions rather than terminal cells,
so resizing reflows the document while keeping the same logical content in
view.

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
    high-quality tables, and any review-oriented workflow.
- Differentiation: build for the AI-review use case — inline comments, math
  and tables as first-class citizens — rather than competing as a
  general-purpose viewer.
- Not finalized: whether later graphics support builds on ratatui-image,
  inline comment design, and live mode design.
- Naming: rejected candidates — makie (collides with Makie.jl), galley
  (collides with an AI-coding CLI and a macOS PDF previewer), inlay ("inlay
  hints" association), tipin (obscure), focal (generic, Ubuntu Focal Fossa).
  raden pairs with the author's urushi project (lacquer-craft lineage).
