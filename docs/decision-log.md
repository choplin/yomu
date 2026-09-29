# Decision Log

This table preserves design decisions whose historical context would otherwise
disappear when linked documents are rewritten in place. Each row records a
durable design rule chosen or revised among meaningful alternatives and why;
implementation and documentation activity is not recorded. Newest first.

| Date | Decision | Current documentation |
| --- | --- | --- |
| 2026-09-29 | Moved terminal UI, event handling, capability discovery, and session restoration to Urushi while keeping Raden's Kitty and sixel overlays inside one direct terminal backend. This removed the Ratatui/Crossterm dependency path without moving Markdown or graphics policy out of Raden. | [`architecture.md`](architecture.md), [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-09-08 | Accepted Markdown from standard input while resolving relative image paths from the current working directory, preserving the same parsed-document and terminal-viewing path as file input. | [`architecture.md`](architecture.md) |
| 2026-09-01 | Made raster dimensions preserve image aspect ratio in terminal-cell geometry so placement does not stretch assets. | [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-08-14 | Made graphics failure recover by rebuilding the complete text projection at the current source anchor; automatic selection uses capability evidence in Kitty, sixel, text order, while explicit unavailable backends fail. | [`design/graphics-fallback.md`](design/graphics-fallback.md), [`design/source-anchored-layout.md`](design/source-anchored-layout.md) |
| 2026-08-14 | Added math as source-aware scene elements using a sandboxed TeX-to-Typst-to-SVG-to-RGBA path with cell-aligned caching and source-text fallback. | [`architecture.md`](architecture.md), [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-08-14 | Added experimental sixel as an immediate-mode renderer over the shared desired scene, using bounded encoded-band reuse and complete redraw semantics rather than Kitty-style placement state. | [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-08-13 | Added tables as source-aware scene elements with deterministic content-derived layout, cell source mapping, bounded rasterization, and complete text fallback. | [`architecture.md`](architecture.md), [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-08-13 | Introduced a backend-neutral raster scene for local images and large headings, latest-request-wins logical rendering, bounded work, and readable fallback. | [`architecture.md`](architecture.md), [`design/graphics-fallback.md`](design/graphics-fallback.md) |
| 2026-08-12 | Established UTF-8 source byte ranges as the identity carried through width-dependent layout and used source positions, rather than rendered rows, to restore the viewport. | [`design/source-anchored-layout.md`](design/source-anchored-layout.md) |
